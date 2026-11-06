using System.Collections.Generic;
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

    // ── Every account's plugin settings cards (third-party.md § The principal model →
    //    Hosted principals) ──

    /// <summary>The connected-apps machine the card verbs go through: records each call and
    /// plays back a scripted page error.</summary>
    private sealed class CardMachine : uniffi.fauna_client_connected_apps.ConnectedAppsMachineFakeBase
    {
        public readonly List<string> Calls = new();
        public uniffi.fauna_core.LocalizedText? Error;

        public override Task BindPlugin(byte[] principalId)
        {
            Calls.Add($"BindPlugin:{System.Convert.ToHexString(principalId).ToLowerInvariant()}");
            return Task.CompletedTask;
        }

        public override Task RevokePrincipal(byte[] principalId)
        {
            Calls.Add($"RevokePrincipal:{System.Convert.ToHexString(principalId).ToLowerInvariant()}");
            return Task.CompletedTask;
        }

        public override uniffi.fauna_client_connected_apps.ConnectedAppsSnapshot Snapshot() => new(
            @loaded: true,
            @requests: System.Array.Empty<uniffi.fauna_atproto_settings_machine.ConsentCardRow>(),
            @principals: System.Array.Empty<uniffi.fauna_client_connected_apps.ConnectedAppRow>(),
            @blocked: System.Array.Empty<uniffi.fauna_client_connected_apps.BlockedAppRow>(),
            @externalAppsOff: false,
            @error: Error);
    }

    private static (BridgesViewModel Vm, CardMachine Machine) CardVm(MockNestRpcClient rpc)
    {
        var machine = new CardMachine();
        var vm = new BridgesViewModel(
            rpc, () => Task.FromResult<uniffi.fauna_client_connected_apps.IConnectedAppsMachine>(machine));
        return (vm, machine);
    }

    [Fact]
    public async Task LoadPluginCards_ReadsEveryAccountsCardsWithoutTheAdminGate()
    {
        var rpc = new MockNestRpcClient
        {
            NextAmIAdmin = false,
            NextPluginCards ={ MockNestRpcClient.MakePluginCard("aa01", "Hello Plugin", "https://plugins.example/hello.json") },
        };
        var (vm, _) = CardVm(rpc);
        Assert.Null(vm.PluginCards);   // the three-state list: nothing before the read returns

        await vm.LoadPluginCardsAsync();

        var card = Assert.Single(vm.PluginCards!);
        Assert.Equal("Hello Plugin", card.@name);
        Assert.Contains("PluginsAvailable", rpc.Calls);
        Assert.DoesNotContain("AmIAdmin", rpc.Calls);
    }

    [Fact]
    public async Task PressPluginCard_Unbound_BindsThroughTheMachineShowsTheHintAndReReads()
    {
        var rpc = new MockNestRpcClient();
        var (vm, machine) = CardVm(rpc);
        var card = MockNestRpcClient.MakePluginCard("aa01", "Hello Plugin", "https://plugins.example/hello.json");

        await vm.PressPluginCardAsync(card);

        Assert.Equal(new[] { "BindPlugin:aa01" }, machine.Calls);
        Assert.True(vm.PluginBindOpened);
        Assert.Null(vm.ErrorMessage);
        Assert.Contains("PluginsAvailable", rpc.Calls);
    }

    [Fact]
    public async Task PressPluginCard_Bound_RevokesTheBindingRowNotTheInstallRow()
    {
        var rpc = new MockNestRpcClient();
        var (vm, machine) = CardVm(rpc);
        var card = MockNestRpcClient.MakePluginCard(
            "aa01", "Hello Plugin", "https://plugins.example/hello.json", bindingPrincipalIdHex: "bb02");

        await vm.PressPluginCardAsync(card);

        Assert.Equal(new[] { "RevokePrincipal:bb02" }, machine.Calls);
        Assert.False(vm.PluginBindOpened);
    }

    [Fact]
    public async Task PressPluginCard_MachineError_ShowsItAndShowsNoHint()
    {
        var rpc = new MockNestRpcClient();
        var (vm, machine) = CardVm(rpc);
        machine.Error = new uniffi.fauna_core.LocalizedText("connected_apps.error_bind", new Dictionary<string, string>());
        var card = MockNestRpcClient.MakePluginCard("aa01", "Hello Plugin", "https://plugins.example/hello.json");

        await vm.PressPluginCardAsync(card);

        Assert.False(vm.PluginBindOpened);
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        Assert.DoesNotContain("PluginsAvailable", rpc.Calls);
    }

    [Fact]
    public async Task SetPluginSetting_WritesThisAccountsSettingThroughThePluginKindAndReReads()
    {
        var rpc = new MockNestRpcClient();
        var (vm, _) = CardVm(rpc);

        await vm.SetPluginSettingAsync("aa01", BridgeSettingValue.Text("greeting", "hi there"));

        Assert.Equal("aa01", rpc.LastPluginSetting!.Value.PrincipalIdHex);
        Assert.Equal(BridgeSettingValue.Text("greeting", "hi there"), rpc.LastPluginSetting!.Value.Value);
        Assert.Contains("PluginsAvailable", rpc.Calls);
        Assert.DoesNotContain("BridgesSetSettings", rpc.Calls);
    }

    [Fact]
    public void PluginCard_SettingsAreTheBridgeRowsAndTheButtonCarriesClientIdAndBound()
    {
        var card = MockNestRpcClient.MakePluginCard(
            "aa01", "Hello Plugin", "https://plugins.example/hello.json", bindingPrincipalIdHex: "bb02",
            settings: new[]
            {
                new uniffi.fauna_ffi.FfiBridgeSetting(
                    "greeting", "Greeting", "string", new uniffi.fauna_ffi.FfiCborValue.Text("hello"), null),
            });

        var setting = Assert.Single(BridgesViewModel.PluginCardSettings(card));
        Assert.Equal(new BridgeSetting("greeting", "Greeting", "string", null, "hello"), setting);
        // The one settings renderer paints a `string` setting as an editable text box.
        Assert.Equal(
            uniffi.fauna_ffi.FfiSettingControl.Text,
            uniffi.fauna_ffi.FaunaFfiMethods.SettingControl(setting.SettingType));
        Assert.Equal(
            "client_id=https://plugins.example/hello.json;bound=true",
            BridgesViewModel.PluginCardActionAttributes(card));
    }

    // ── The admin's hosted plugins (bridges.md § Hosted plugins on this page) ──

    [Fact]
    public async Task LoadPluginsAsync_NonAdmin_NeverListsAndPaintsNoSection()
    {
        var rpc = new MockNestRpcClient { NextAmIAdmin = false };
        var vm = new BridgesViewModel(rpc);

        await vm.LoadPluginsAsync();

        Assert.False(vm.IsAdmin);
        Assert.Null(vm.Plugins);
        Assert.DoesNotContain("PluginsList", rpc.Calls);
    }

    [Fact]
    public async Task LoadPluginsAsync_FailedGateRead_FailsClosed()
    {
        var rpc = new MockNestRpcClient { NextAmIAdmin = true, NextError = "offline" };
        var vm = new BridgesViewModel(rpc);

        await vm.LoadPluginsAsync();

        Assert.False(vm.IsAdmin);
        Assert.Null(vm.Plugins);
    }

    [Fact]
    public async Task LoadPluginsAsync_Admin_ReadsTheList()
    {
        var rpc = new MockNestRpcClient
        {
            NextAmIAdmin = true,
            NextPlugins = { MockNestRpcClient.MakeInstalledPlugin("aa01", "Hello Plugin", "https://plugins.example/hello.json") },
        };
        var vm = new BridgesViewModel(rpc);
        Assert.Null(vm.Plugins);   // the three-state list: unread before the read returns

        await vm.LoadPluginsAsync();

        Assert.True(vm.IsAdmin);
        var row = Assert.Single(vm.Plugins!);
        Assert.Equal("Hello Plugin", row.name);
    }

    [Fact]
    public async Task LoadPluginsAsync_FailedList_ShowsTheErrorAndLeavesItUnread()
    {
        var rpc = new MockNestRpcClient
        {
            NextAmIAdmin = true,
            NextPluginsError = new System.InvalidOperationException("refused"),
        };
        var vm = new BridgesViewModel(rpc);

        await vm.LoadPluginsAsync();

        Assert.True(vm.IsAdmin);
        Assert.Null(vm.Plugins);
        Assert.NotNull(vm.ErrorMessage);
    }

    [Fact]
    public async Task InstallPluginAsync_SendsTheTrimmedUrlAndShowsTheHint()
    {
        var rpc = new MockNestRpcClient { NextAmIAdmin = true };
        var vm = new BridgesViewModel(rpc);

        Assert.True(await vm.InstallPluginAsync("  https://plugins.example/hello.json  "));

        Assert.Equal("https://plugins.example/hello.json", rpc.LastPluginInstallUrl);
        Assert.True(vm.PluginInstallOpened);
    }

    [Fact]
    public async Task InstallPluginAsync_Refused_ShowsTheErrorAndNoHint()
    {
        var rpc = new MockNestRpcClient { NextPluginsError = new System.InvalidOperationException("bad document") };
        var vm = new BridgesViewModel(rpc);

        Assert.False(await vm.InstallPluginAsync("https://plugins.example/hello.json"));

        Assert.False(vm.PluginInstallOpened);
        Assert.NotNull(vm.ErrorMessage);
    }

    [Fact]
    public async Task InstallPluginAsync_BlankUrl_SendsNothing()
    {
        var rpc = new MockNestRpcClient();
        var vm = new BridgesViewModel(rpc);

        Assert.False(await vm.InstallPluginAsync("   "));

        Assert.DoesNotContain("PluginsInstall", rpc.Calls);
    }

    [Fact]
    public async Task PressUninstall_FirstPressArms_SecondUninstallsAndReReads()
    {
        var rpc = new MockNestRpcClient
        {
            NextAmIAdmin = true,
            PersistPlugins = true,
            NextPlugins = { MockNestRpcClient.MakeInstalledPlugin("aa01", "Hello Plugin", "https://plugins.example/hello.json") },
        };
        var vm = new BridgesViewModel(rpc);
        await vm.LoadPluginsAsync();

        await vm.PressUninstallPluginAsync("aa01");
        Assert.Equal("aa01", vm.PluginUninstallArmed);
        Assert.DoesNotContain("PluginsUninstall", rpc.Calls);

        await vm.PressUninstallPluginAsync("aa01");
        Assert.Null(vm.PluginUninstallArmed);
        Assert.Equal("aa01", rpc.LastPluginUninstall);
        Assert.Empty(vm.Plugins!);   // the re-read shows what the nest persisted
    }

    [Fact]
    public async Task CancelUninstall_DisarmsWithoutUninstalling()
    {
        var rpc = new MockNestRpcClient { NextAmIAdmin = true };
        var vm = new BridgesViewModel(rpc);

        await vm.PressUninstallPluginAsync("aa01");
        vm.CancelUninstallPlugin();

        Assert.Null(vm.PluginUninstallArmed);
        Assert.DoesNotContain("PluginsUninstall", rpc.Calls);
    }

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
    // ── Follow requests (bridges.md § Follow requests) ──

    private static BridgeInfo RequestsBridge(string id, bool linked = true, bool supports = true) => new(
        Id: id, DisplayName: id, Available: true, Linked: linked,
        Identity: null, Mode: null,
        LinkModes: System.Array.Empty<BridgeLinkMode>(),
        Settings: System.Array.Empty<BridgeSetting>(),
        SupportsFollowRequests: supports);

    [Fact]
    public async Task Load_ReadsTheWaitingRequestsOfALinkedBridgeThatDeclaresThem()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { RequestsBridge("activitypub") },
        };
        rpc.FollowRequests["activitypub"] = new List<BridgeFollowRequest>
        {
            new("https://a.example/users/bob", "Bob (@bob@a.example)"),
            new("https://a.example/users/carol", "@carol@a.example"),
        };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "activitypub" };

        await vm.LoadCommand.ExecuteAsync(null);

        var rows = vm.Bridges.Single().FollowRequests!;
        Assert.Equal(new[] { "Bob (@bob@a.example)", "@carol@a.example" }, rows.Select(r => r.Display));
        Assert.Null(vm.ErrorMessage);
    }

    [Theory]
    [InlineData(false, true)]   // not linked: no account to hold requests for
    [InlineData(true, false)]   // the provider declares no follow requests
    public async Task Load_AsksForNoRequestsWhereTheCardRendersNoSection(bool linked, bool supports)
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { RequestsBridge("activitypub", linked, supports) },
        };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "activitypub" };

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.DoesNotContain("BridgesListFollowRequests", rpc.Calls);
        Assert.Empty(vm.Bridges.Single().FollowRequests ?? System.Array.Empty<BridgeFollowRequest>());
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public async Task AnswerFollowRequest_ForwardsTheRowIdAndReReads(bool approve)
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { RequestsBridge("activitypub") },
        };
        rpc.FollowRequests["activitypub"] = new List<BridgeFollowRequest>
        {
            new("https://a.example/users/bob", "@bob@a.example"),
            new("https://a.example/users/carol", "@carol@a.example"),
        };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "activitypub" };
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.AnswerFollowRequestAsync("activitypub", "https://a.example/users/bob", approve);

        Assert.Equal(("activitypub", "https://a.example/users/bob", approve), rpc.LastFollowRequestAnswer);
        // Non-optimistic: the row is gone because the re-read no longer lists it.
        var rows = vm.Bridges.Single().FollowRequests!;
        Assert.Equal(new[] { "@carol@a.example" }, rows.Select(r => r.Display));
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task AnswerFollowRequest_Failure_SetsErrorMessage()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { RequestsBridge("activitypub") },
        };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "activitypub" };
        await vm.LoadCommand.ExecuteAsync(null);

        rpc.NextError = "resolve failed";
        await vm.AnswerFollowRequestAsync("activitypub", "x", approve: true);

        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }
    // ── Follows on the card (bridges.md § Follows, § User actions) ──

    private static BridgeInfo FollowsBridge(string id, bool linked = true, bool supports = true) => new(
        Id: id, DisplayName: id, Available: true, Linked: linked,
        Identity: null, Mode: null,
        LinkModes: System.Array.Empty<BridgeLinkMode>(),
        Settings: System.Array.Empty<BridgeSetting>(),
        SupportsFollows: supports);

    [Fact]
    public async Task Load_ReadsTheFollowsOfALinkedBridgeThatSupportsThem()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { FollowsBridge("activitypub") },
            NextFollows = new[] { new BridgeFollow("https://a.example/users/alice", "Alice") },
        };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "activitypub" };

        await vm.LoadCommand.ExecuteAsync(null);

        var follow = Assert.Single(vm.Bridges.Single().Follows!);
        Assert.Equal(("https://a.example/users/alice", "Alice"), (follow.Id, follow.Petname));
    }

    [Theory]
    [InlineData(false, true)]
    [InlineData(true, false)]
    public async Task Load_ReadsNoFollowsWhereTheCardRendersNoList(bool linked, bool supports)
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { FollowsBridge("activitypub", linked, supports) },
        };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "activitypub" };

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.DoesNotContain("BridgesListFollows", rpc.Calls);
    }

    [Fact]
    public async Task AddFollow_SendsTheIdAndNicknameToThatBridgeAndReReads()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { FollowsBridge("activitypub") },
        };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "activitypub" };
        await vm.LoadCommand.ExecuteAsync(null);
        var readsBefore = rpc.Calls.Count(c => c == "BridgesList");

        await vm.AddFollowAsync("activitypub", "https://a.example/users/alice", "Alice");

        Assert.Equal(("activitypub", "https://a.example/users/alice", "Alice"), rpc.LastAddFollow);
        Assert.Equal(readsBefore + 1, rpc.Calls.Count(c => c == "BridgesList"));
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task RemoveFollow_RemovesThatFollowAndReReads()
    {
        var rpc = new MockNestRpcClient
        {
            NextBridges = new[] { FollowsBridge("activitypub") },
        };
        var vm = new BridgesViewModel(rpc) { SingleBridgeId = "activitypub" };
        await vm.LoadCommand.ExecuteAsync(null);
        var readsBefore = rpc.Calls.Count(c => c == "BridgesList");

        await vm.RemoveFollowAsync("activitypub", "https://a.example/users/alice");

        Assert.Equal(("activitypub", "https://a.example/users/alice"), rpc.LastRemoveFollow);
        Assert.Equal(readsBefore + 1, rpc.Calls.Count(c => c == "BridgesList"));
    }

    [Fact]
    public void IdentityLine_IsTheLabelAndTheDisplay()
    {
        var bridge = FollowsBridge("activitypub") with { Identity = "@me@a.example", IdentityLabel = "Handle" };
        Assert.Equal("Handle: @me@a.example", bridge.IdentityLine);
        Assert.Null((bridge with { Identity = null }).IdentityLine);
    }
}
