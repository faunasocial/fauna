using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the <c>admin-aliases</c> page VM (admin.md § 4 —
/// admin external forwarders, Kind 7), over a <see cref="FakeForwarderMachine"/>
/// implementing the same UniFFI <see cref="IForwarderMachine"/> seam the real
/// shared <c>ForwarderMachine</c> does (no live nest / FlaUI — FlaUI flakes on
/// win-arm64, so this VM is the windows deterministic gate). The fake mirrors the
/// Rust machine's <c>libs/fauna-client-mail-settings/src/forwarders.rs</c> FakeNest
/// contract: project rows + hosted domains, reject a (domain, local-part)
/// collision, delete by alias id.
/// </summary>
public class AdminAliasesViewModelTests
{
    [Fact]
    public async Task Load_ProjectsForwardersAndDomains()
    {
        var fake = new FakeForwarderMachine(
            forwarders: new[] { FakeForwarderMachine.Row("ab", "example.com", "info", "real@example.net") },
            domains: new[] { "example.com", "other.test" });
        var vm = new AdminAliasesViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        var row = Assert.Single(vm.Forwarders);
        Assert.Equal("info@example.com", row.Address);
        Assert.Equal("real@example.net", row.Target);
        Assert.Equal("ab", row.AliasIdHex);
        // The hosted-domain picker options (a forwarder must live on one).
        Assert.Equal(new[] { "example.com", "other.test" }, vm.Domains);
        Assert.Null(vm.Error);
        Assert.False(vm.IsLoading);
    }

    [Fact]
    public async Task Create_AddsForwarder_AndRelists()
    {
        var fake = new FakeForwarderMachine(domains: new[] { "example.com" });
        var vm = new AdminAliasesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.CreateAsync("example.com", "sales", "team@offsite.example");

        var row = Assert.Single(vm.Forwarders);
        Assert.Equal("sales@example.com", row.Address);
        Assert.Equal("team@offsite.example", row.Target);
        Assert.Null(vm.Error);
    }

    [Theory]
    [InlineData(null, "info", "x@y.example")]     // no hosted domain selected yet
    [InlineData("example.com", "", "x@y.example")]  // empty local part
    [InlineData("example.com", "info", "   ")]      // blank target
    public async Task Create_BlankField_IsNoOp(string? domain, string? pattern, string? target)
    {
        var fake = new FakeForwarderMachine(domains: new[] { "example.com" });
        var vm = new AdminAliasesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.CreateAsync(domain, pattern, target);

        Assert.Empty(vm.Forwarders);
        // The guard returns before any action reaches the machine.
        Assert.Empty(fake.Dispatched);
    }

    [Fact]
    public async Task Create_Collision_SurfacesError_AndKeepsList()
    {
        var fake = new FakeForwarderMachine(
            forwarders: new[] { FakeForwarderMachine.Row("01", "example.com", "info", "a@b.example") },
            domains: new[] { "example.com" });
        var vm = new AdminAliasesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.CreateAsync("example.com", "info", "c@d.example");

        // The nest-class rejection rides the snapshot error → admin-aliases-action-error.
        Assert.Equal("conflicts_with_existing_alias", vm.Error);
        var row = Assert.Single(vm.Forwarders); // list left intact
        Assert.Equal("a@b.example", row.Target);
    }

    [Fact]
    public async Task Delete_RemovesForwarder()
    {
        var fake = new FakeForwarderMachine(
            forwarders: new[] { FakeForwarderMachine.Row("22", "example.com", "info", "a@b.example") },
            domains: new[] { "example.com" });
        var vm = new AdminAliasesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);
        var id = Assert.Single(vm.Forwarders).AliasIdHex;

        await vm.DeleteAsync(id);

        Assert.Empty(vm.Forwarders);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task Load_Failure_SurfacesErrorToErrorProperty()
    {
        var fake = new FakeForwarderMachine { ThrowOnHydrate = true };
        var vm = new AdminAliasesViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.NotNull(vm.Error);
        Assert.Empty(vm.Forwarders);
        Assert.False(vm.IsLoading);
    }
}

/// <summary>
/// In-memory <see cref="IForwarderMachine"/> for the VM unit tests — the C# peer of
/// the Rust <c>forwarders.rs</c> FakeNest. Models the forwarder table + hosted-domain
/// list, refuses a <c>(local_domain, pattern)</c> collision (recording the error into
/// the snapshot and throwing, exactly as the real machine's dispatch does), and
/// deletes by alias id. <c>Hydrate</c> can be made to fail via
/// <see cref="ThrowOnHydrate"/> (the load-error path).
/// </summary>
internal sealed class FakeForwarderMachine : ForwarderMachineFakeBase
{
    private readonly List<ForwarderView> _forwarders = new();
    private readonly List<string> _domains = new();
    private string? _error;
    private int _idCounter = 0x10;

    /// <summary>Make <see cref="Hydrate"/> throw (the list_forwarders-failed path).</summary>
    public bool ThrowOnHydrate { get; set; }

    /// <summary>Every action the VM dispatched, in order — lets a test assert the
    /// add-form guard returned before reaching the machine.</summary>
    public List<ForwarderAction> Dispatched { get; } = new();

    public FakeForwarderMachine(
        IEnumerable<ForwarderView>? forwarders = null, IEnumerable<string>? domains = null)
    {
        if (forwarders is not null) _forwarders.AddRange(forwarders);
        if (domains is not null) _domains.AddRange(domains);
    }

    /// <summary>Build a row the way the shared <c>ForwarderView::from(AliasRow)</c>
    /// does — address is <c>&lt;pattern&gt;@&lt;local_domain&gt;</c>.</summary>
    public static ForwarderView Row(string aliasIdHex, string domain, string pattern, string target) =>
        new(aliasIdHex, domain, pattern, $"{pattern}@{domain}", target);

    public override Task Hydrate()
    {
        if (ThrowOnHydrate)
        {
            throw new InvalidOperationException("list_forwarders failed");
        }
        return Task.CompletedTask;
    }

    public override Task Dispatch(ForwarderAction action)
    {
        Dispatched.Add(action);
        _error = null; // dispatch clears the prior error first (mirrors the machine)
        switch (action)
        {
            case ForwarderAction.Refresh:
                break;
            case ForwarderAction.Create c:
                if (_forwarders.Any(f => f.localDomain == c.localDomain && f.pattern == c.pattern))
                {
                    _error = "conflicts_with_existing_alias";
                    throw new InvalidOperationException(_error);
                }
                var id = (++_idCounter).ToString("x2").PadLeft(32, '0');
                _forwarders.Add(Row(id, c.localDomain, c.pattern, c.forwardTarget));
                break;
            case ForwarderAction.Delete d:
                _forwarders.RemoveAll(f => f.aliasIdHex == d.aliasIdHex);
                break;
        }
        return Task.CompletedTask;
    }

    public override ForwardersSnapshot Snapshot() =>
        new(_forwarders.ToArray(), _domains.ToArray(), ForwarderStatus.Idle, _error);
}
