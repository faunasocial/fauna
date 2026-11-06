using Xunit;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the <c>admin-mail-reports</c> page VM
/// (dmarc-reporting.md § Admin pane summary surface), over a
/// <see cref="FakeDmarcReportsMachine"/> implementing the same UniFFI
/// <see cref="IDmarcReportsMachine"/> seam the real shared machine does (no live
/// nest / FlaUI — FlaUI flakes on win-arm64, so this VM is the deterministic gate).
/// </summary>
public class AdminMailReportsViewModelTests
{
    private static readonly uint[] Windows = { 7, 30, 90 };

    [Fact]
    public async Task BeforeTheFirstRead_NothingButFiltersPaint()
    {
        var fake = new FakeDmarcReportsMachine { Loaded = false };
        var vm = new AdminMailReportsViewModel(fake, Windows);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.Loaded);
        Assert.Equal(string.Empty, vm.MessagesText);
        Assert.False(vm.Empty);
        Assert.Empty(vm.Reports);
        Assert.Equal(new[] { "7", "30", "90" }, vm.WindowOptions.Select(o => o.Value));
    }

    [Fact]
    public async Task Load_ProjectsTotalsRowsAndPickers()
    {
        var fake = new FakeDmarcReportsMachine();
        var vm = new AdminMailReportsViewModel(fake, Windows);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.Loaded);
        Assert.Equal(Strings.Format("admin/mail_reports_page/messages", 15, 1, 1), vm.MessagesText);
        Assert.Equal(
            Strings.Format("admin/mail_reports_page/alignment", 12, 80, 15, 100, 9, 60), vm.AlignmentText);
        Assert.Equal(Strings.Format("admin/mail_reports_page/dispositions", 15, 0, 0), vm.DispositionsText);
        var source = Assert.Single(vm.Sources);
        Assert.Equal("192.0.2.10", source.Value);
        Assert.Equal(Strings.Format("admin/mail_reports_page/row_messages", 15), source.CountText);
        var row = Assert.Single(vm.Reports);
        Assert.Equal("example.org", row.OurDomain);
        Assert.Equal(Strings.Format("admin/mail_reports_page/report_pass", 12, 15), row.Pass);
        Assert.Equal(
            Strings.Format("admin/mail_reports_page/report_period",
                FaunaFfiMethods.FormatUnixLocalDate(1_700_000_000), FaunaFfiMethods.FormatUnixLocalDate(1_700_086_400)),
            row.Period);
        Assert.Equal("7", vm.SelectedWindow);
        // Every domain first, with the raw value "".
        Assert.Equal(new[] { "", "example.org", "other.test" }, vm.DomainOptions.Select(o => o.Value));
        Assert.False(vm.Empty);
        Assert.False(vm.Truncated);
        Assert.Null(vm.RawXml);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task EmptyWindow_PaintsTheEmptyState()
    {
        var fake = new FakeDmarcReportsMachine { Reports = Array.Empty<DmarcReportRowView>() };
        var vm = new AdminMailReportsViewModel(fake, Windows);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.Loaded);
        Assert.True(vm.Empty);
        Assert.Empty(vm.Reports);
    }

    [Fact]
    public async Task OpenReport_ShowsTheRawXml_AndAFilterChangeClosesIt()
    {
        var fake = new FakeDmarcReportsMachine();
        var vm = new AdminMailReportsViewModel(fake, Windows);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.OpenReportAsync(Assert.Single(vm.Reports));
        Assert.Equal("<feedback/>", vm.RawXml);
        var open = Assert.IsType<DmarcReportsAction.OpenReport>(fake.Dispatched.Last());
        Assert.Equal(("example.org", "reporter.test", "r-1"), (open.ourDomain, open.submitter, open.reportId));

        await vm.SetDomainAsync("other.test");
        Assert.Null(vm.RawXml);
        Assert.Equal("other.test", vm.SelectedDomain);
        Assert.Equal("other.test", Assert.IsType<DmarcReportsAction.SetDomain>(fake.Dispatched.Last()).domain);

        await vm.SetDomainAsync(AdminMailReportsViewModel.EveryDomain);
        Assert.Null(Assert.IsType<DmarcReportsAction.SetDomain>(fake.Dispatched.Last()).domain);
        Assert.Equal(AdminMailReportsViewModel.EveryDomain, vm.SelectedDomain);
    }

    [Fact]
    public async Task SetWindow_DispatchesAndSelects()
    {
        var fake = new FakeDmarcReportsMachine();
        var vm = new AdminMailReportsViewModel(fake, Windows);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SetWindowAsync(30);

        Assert.Equal(30u, Assert.IsType<DmarcReportsAction.SetWindow>(fake.Dispatched.Last()).days);
        Assert.Equal("30", vm.SelectedWindow);
    }

    [Fact]
    public async Task Truncated_PaintsOnlyWhileTheCapWasHit()
    {
        var fake = new FakeDmarcReportsMachine { Truncated = true };
        var vm = new AdminMailReportsViewModel(fake, Windows);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.Truncated);
    }

    [Fact]
    public async Task HydrateFailure_SurfacesTheError()
    {
        var fake = new FakeDmarcReportsMachine { ThrowOnHydrate = true, Loaded = false };
        var vm = new AdminMailReportsViewModel(fake, Windows);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(string.IsNullOrEmpty(vm.Error));
        Assert.False(vm.IsLoading);
    }
}

/// <summary>In-memory <see cref="IDmarcReportsMachine"/>: one report from one peer
/// on <c>example.org</c>, the window / domain / opened-report state the real machine
/// keeps (a filter change closes the opened report).</summary>
internal sealed class FakeDmarcReportsMachine : DmarcReportsMachineFakeBase
{
    private uint _window = 7;
    private string? _domain;
    private DmarcOpenedReport? _opened;

    public bool Loaded { get; set; } = true;
    public bool Truncated { get; set; }
    public bool ThrowOnHydrate { get; set; }
    public DmarcReportRowView[] Reports { get; set; } =
    {
        new("example.org", "reporter.test", "r-1", 1_700_000_000, 1_700_086_400, 1_700_090_000, 15, 12),
    };
    public List<DmarcReportsAction> Dispatched { get; } = new();

    public override Task Hydrate()
    {
        if (ThrowOnHydrate) throw new InvalidOperationException("list_dmarc_aggregate_reports failed");
        return Task.CompletedTask;
    }

    public override Task Dispatch(DmarcReportsAction action)
    {
        Dispatched.Add(action);
        switch (action)
        {
            case DmarcReportsAction.SetWindow w:
                _window = w.days;
                _opened = null;
                break;
            case DmarcReportsAction.SetDomain d:
                _domain = d.domain;
                _opened = null;
                break;
            case DmarcReportsAction.OpenReport o:
                _opened = new DmarcOpenedReport(o.ourDomain, o.submitter, o.reportId, "<feedback/>");
                break;
        }
        return Task.CompletedTask;
    }

    public override DmarcReportsSnapshot Snapshot() => new(
        _window,
        _domain,
        new[] { "example.org", "other.test" },
        Loaded,
        new DmarcWindowTotals(15, 1, 1, 12, 80, 15, 100, 9, 60, 15, 0, 0),
        new[] { new DmarcCountRow("192.0.2.10", 15) },
        new[] { new DmarcCountRow("example.org", 15) },
        Reports,
        Truncated,
        _opened,
        DmarcReportsStatus.Idle,
        null);
}
