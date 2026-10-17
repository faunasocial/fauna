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
/// The admin <c>admin-dashboard</c> page VM — a renderer of the shared card set
/// (<c>admin_dashboard_snapshot()</c>, docs/goal/behavior/admin.md § 1.
/// Dashboard → <i>The card set</i>). Mirrors the invariants tui's
/// <c>admin/dashboard.rs</c> and android's <c>AdminDashboardContentTest</c> pin:
/// <list type="number">
/// <item><b>The cards are the snapshot's, in its order</b> — the VM resolves
/// each label and value and adds, drops or reorders nothing.</item>
/// <item><b>A failed read drops only its own cards</b> — the snapshot's
/// <c>error</c> shows through <c>admin.dashboard.load_error</c> while the
/// other reads' cards still paint.</item>
/// <item><b>Each load is fresh truth</b> — a reload replaces the cards and
/// clears a stale error.</item>
/// </list>
///
/// <para>[Collection("StringsGlobal")] because card labels and values resolve
/// shared <c>LocalizedText</c> through <see cref="Strings"/>, whose localizer is
/// process-global; this class installs its own <see cref="FakeLocalizer"/>
/// mapping the real <c>admin/dashboard/*</c> templates.</para>
/// </summary>
[Collection("StringsGlobal")]
public class AdminDashboardViewModelTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["admin/view/registered_users"] = "Registered Users",
            ["admin/dashboard/suspended"] = "Suspended",
            ["admin/dashboard/secure_connections"] = "Secure Connections",
            ["admin/dashboard/secure_on"] = "On",
            ["admin/dashboard/version"] = "Version",
            ["admin/dashboard/verbatim"] = "{text}",
            ["admin/dashboard/load_error"] = "Failed to load admin stats: {message}",
            ["errors/http_error"] = "HTTP error: {message}",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public AdminDashboardViewModelTests() => Strings.Initialize(new FakeLocalizer());

    private static LocalizedText Key(string key) => new(key, new Dictionary<string, string>());

    private static LocalizedText Verbatim(string text) =>
        new("admin.dashboard.verbatim", new Dictionary<string, string> { ["text"] = text });

    private static FfiStatCard Card(LocalizedText label, LocalizedText value) => new(label, value);

    private static FfiAdminDashboardSnapshot Snapshot(string? error = null, params FfiStatCard[] cards) =>
        new(cards, error);

    private static List<(string, string)> Painted(AdminDashboardViewModel vm) =>
        vm.Cards.Select(c => (c.Label, c.Value)).ToList();

    [Fact]
    public async Task LoadAsync_PaintsTheSnapshotsCards_InItsOrder_Resolved()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminDashboardSnapshot = Snapshot(null,
                Card(Key("admin.view.registered_users"), Verbatim("3")),
                Card(Key("admin.dashboard.suspended"), Verbatim("0")),
                // A tier card: the tier's own name is the label.
                Card(Verbatim("free"), Verbatim("3")),
                Card(Key("admin.dashboard.secure_connections"), Key("admin.dashboard.secure_on")),
                Card(Key("admin.dashboard.version"), Verbatim("1.2.3"))),
        };
        var vm = new AdminDashboardViewModel(rpc);

        await vm.LoadAsync();

        Assert.Equal(new List<(string, string)>
        {
            ("Registered Users", "3"),
            ("Suspended", "0"),
            ("free", "3"),
            ("Secure Connections", "On"),
            ("Version", "1.2.3"),
        }, Painted(vm));
        Assert.True(vm.HasLoaded);
        Assert.Null(vm.ErrorMessage);
        Assert.Contains("AdminDashboardSnapshot", rpc.Calls);
    }

    [Fact]
    public async Task LoadAsync_AFailedRead_ShowsLoadError_AndKeepsTheOtherReadsCards()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminDashboardSnapshot = Snapshot("fauna.admin.stats: boom",
                Card(Key("admin.dashboard.version"), Verbatim("1.2.3"))),
        };
        var vm = new AdminDashboardViewModel(rpc);

        await vm.LoadAsync();

        Assert.Equal("Failed to load admin stats: fauna.admin.stats: boom", vm.ErrorMessage);
        Assert.Equal(new List<(string, string)> { ("Version", "1.2.3") }, Painted(vm));
    }

    [Fact]
    public async Task LoadAsync_TheCallItselfFails_ShowsLoadError_AndPaintsNoCards()
    {
        var rpc = new MockNestRpcClient { NextError = "unreachable" };
        var vm = new AdminDashboardViewModel(rpc);

        await vm.LoadAsync();

        Assert.Equal("Failed to load admin stats: HTTP error: unreachable", vm.ErrorMessage);
        Assert.Empty(vm.Cards);
        Assert.False(vm.HasLoaded);
    }

    [Fact]
    public async Task LoadAsync_Reload_ReplacesTheCards_AndClearsAStaleError()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminDashboardSnapshot = Snapshot("fauna.nest.info: down",
                Card(Key("admin.view.registered_users"), Verbatim("1"))),
        };
        var vm = new AdminDashboardViewModel(rpc);
        await vm.LoadAsync();
        Assert.NotNull(vm.ErrorMessage);

        rpc.NextAdminDashboardSnapshot = Snapshot(null,
            Card(Key("admin.view.registered_users"), Verbatim("2")),
            Card(Key("admin.dashboard.version"), Verbatim("1.2.4")));
        await vm.LoadAsync();

        Assert.Null(vm.ErrorMessage);
        Assert.Equal(new List<(string, string)>
        {
            ("Registered Users", "2"),
            ("Version", "1.2.4"),
        }, Painted(vm));
    }
}
