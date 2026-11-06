using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The admin <c>admin-mail-reports</c> page (dmarc-reporting.md § Admin pane
/// summary surface). A dumb renderer over the shared <c>DmarcReportsMachine</c>
/// (<c>libs/fauna-client-mail-settings/src/dmarc_reports.rs</c>, exposed by
/// <c>libs/fauna-ffi/src/mail_admin.rs</c> <c>build_dmarc_reports_machine</c>)
/// through its UniFFI <see cref="IDmarcReportsMachine"/> seam — so this VM is
/// unit-testable against a fake (FlaUI flakes on win-arm64, so the VM is the
/// deterministic gate). The machine owns the WS-RPC, the window fold and every
/// percentage; this VM only words the snapshot with
/// <c>admin/mail_reports_page/*</c>, formats the report period with the shared
/// <c>FormatUnixLocalDate</c>, and maps the gestures to <c>DmarcReportsAction</c>.
/// Before <c>loaded</c> only the heading, filters and refresh paint: the totals,
/// lists and empty state are gated on <see cref="Loaded"/>, so "not read yet"
/// and "no reports" stay apart. Mirrors the tui reference
/// (<c>apps/fauna-tui/src/admin/mail_reports.rs</c>) and the shared FaunaKit view.
/// </summary>
public partial class AdminMailReportsViewModel : ObservableObject
{
    /// <summary>The domain filter's raw value for "every domain" (the machine's <c>None</c>).</summary>
    public const string EveryDomain = "";

    private readonly IDmarcReportsMachine _machine;
    private readonly uint[] _windowDays;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;
    [ObservableProperty] private bool _loaded;
    [ObservableProperty] private string _messagesText = string.Empty;
    [ObservableProperty] private string _alignmentText = string.Empty;
    [ObservableProperty] private string _dispositionsText = string.Empty;
    [ObservableProperty] private bool _truncated;
    [ObservableProperty] private bool _empty;
    [ObservableProperty] private string? _rawXml;
    [ObservableProperty] private string _selectedWindow = string.Empty;
    [ObservableProperty] private string _selectedDomain = EveryDomain;

    /// <summary>The window picker's options: raw value = the day count, label = "Last N days".</summary>
    public ObservableCollection<MailReportsOption> WindowOptions { get; } = new();

    /// <summary>The domain picker's options: every domain first (raw value ""), then each hosted domain.</summary>
    public ObservableCollection<MailReportsOption> DomainOptions { get; } = new();

    /// <summary>The top source IPs (<c>admin-mail-reports-source</c>).</summary>
    public ObservableCollection<MailReportsCountRowVm> Sources { get; } = new();

    /// <summary>The top header-From domains (<c>admin-mail-reports-sender</c>).</summary>
    public ObservableCollection<MailReportsCountRowVm> Senders { get; } = new();

    /// <summary>One row per report, newest received first (<c>admin-mail-reports-report</c>).</summary>
    public ObservableCollection<MailReportRowVm> Reports { get; } = new();

    internal AdminMailReportsViewModel(IDmarcReportsMachine machine, uint[] windowDays)
    {
        _machine = machine;
        _windowDays = windowDays;
    }

    /// <summary>Initial page load — the machine reads the window and the domain list.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            await _machine.Hydrate();
            RenderSnapshot();
        }
        catch (Exception ex)
        {
            RenderSnapshot();
            if (string.IsNullOrEmpty(Error)) Error = Strings.Error(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>The refresh button (<c>admin-mail-reports-refresh-button</c>).</summary>
    public Task RefreshAsync() => DispatchAndRenderAsync(new DmarcReportsAction.Refresh());

    /// <summary>Pick the window by its day count and re-read.</summary>
    public Task SetWindowAsync(uint days) => DispatchAndRenderAsync(new DmarcReportsAction.SetWindow(days));

    /// <summary>Pick one domain, or <see cref="EveryDomain"/>, and re-read (closes an opened report).</summary>
    public Task SetDomainAsync(string domain) =>
        DispatchAndRenderAsync(new DmarcReportsAction.SetDomain(
            string.IsNullOrEmpty(domain) ? null : domain));

    /// <summary>Open one report's XML into the raw view (the row's view button).</summary>
    public Task OpenReportAsync(MailReportRowVm row) =>
        DispatchAndRenderAsync(new DmarcReportsAction.OpenReport(row.OurDomain, row.Submitter, row.ReportId));

    /// <summary>Dispatch one action, then render from the resulting snapshot (the
    /// machine records a failure into the snapshot's <c>error</c>; a thrown
    /// <c>DispatchException</c> is only the fallback when it left none).</summary>
    private async Task DispatchAndRenderAsync(DmarcReportsAction action)
    {
        try
        {
            await _machine.Dispatch(action);
        }
        catch (Exception ex)
        {
            RenderSnapshot();
            if (string.IsNullOrEmpty(Error)) Error = Strings.Error(ex);
            return;
        }
        RenderSnapshot();
    }

    private void RenderSnapshot()
    {
        var snap = _machine.Snapshot();
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        Loaded = snap.loaded;
        Truncated = snap.loaded && snap.truncated;
        Empty = snap.loaded && snap.reports.Length == 0;
        RawXml = snap.opened?.xml;

        SelectedWindow = snap.windowDays.ToString();
        SelectedDomain = snap.ourDomain ?? EveryDomain;
        ReplaceOptions(WindowOptions, _windowDays.Select(d =>
            new MailReportsOption(d.ToString(), Strings.Format("admin/mail_reports_page/window_days", d))));
        ReplaceOptions(DomainOptions, new[]
            {
                new MailReportsOption(EveryDomain, Strings.Get("admin/mail_reports_page/every_domain")),
            }.Concat(snap.localDomains.Select(d => new MailReportsOption(d, d))));

        Sources.Clear();
        Senders.Clear();
        Reports.Clear();
        if (!snap.loaded)
        {
            MessagesText = AlignmentText = DispositionsText = string.Empty;
            return;
        }

        var t = snap.totals;
        MessagesText = Strings.Format("admin/mail_reports_page/messages",
            t.messageCount, t.reportCount, t.submitterCount);
        AlignmentText = Strings.Format("admin/mail_reports_page/alignment",
            t.dmarcPass, t.dmarcPassPercent, t.dkimPass, t.dkimPassPercent, t.spfPass, t.spfPassPercent);
        DispositionsText = Strings.Format("admin/mail_reports_page/dispositions",
            t.dispositionNone, t.dispositionQuarantine, t.dispositionReject);
        foreach (var s in snap.sources) Sources.Add(CountRow(s));
        foreach (var s in snap.senders) Senders.Add(CountRow(s));
        foreach (var r in snap.reports) Reports.Add(ReportRow(r));
    }

    private static void ReplaceOptions(ObservableCollection<MailReportsOption> target, IEnumerable<MailReportsOption> next)
    {
        var list = next.ToList();
        if (target.SequenceEqual(list)) return;
        target.Clear();
        foreach (var o in list) target.Add(o);
    }

    private static MailReportsCountRowVm CountRow(DmarcCountRow r) =>
        new(r.value, Strings.Format("admin/mail_reports_page/row_messages", r.count));

    private static MailReportRowVm ReportRow(DmarcReportRowView r) => new(
        r.ourDomain,
        r.submitter,
        r.reportId,
        Strings.Format("admin/mail_reports_page/report_period",
            FaunaFfiMethods.FormatUnixLocalDate(r.beginAt), FaunaFfiMethods.FormatUnixLocalDate(r.endAt)),
        Strings.Format("admin/mail_reports_page/row_messages", r.messageCount),
        Strings.Format("admin/mail_reports_page/report_pass", r.dmarcPass, r.messageCount));
}

/// <summary>One picker option: the raw value the e2e driver selects by, and the label shown.</summary>
public sealed record MailReportsOption(string Value, string Label);

/// <summary>One top-list row: the source IP / header-From domain and its "N messages" line.</summary>
public sealed record MailReportsCountRowVm(string Value, string CountText);

/// <summary>One report row (<c>admin-mail-reports-report</c>): the leaves it renders plus
/// the (our domain, submitter, report id) key its view button opens.</summary>
public sealed record MailReportRowVm(
    string OurDomain, string Submitter, string ReportId, string Period, string Messages, string Pass);
