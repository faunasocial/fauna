using System;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// One training-history row (<c>mail-spam-training-history-list-item</c>),
/// projected from the shared machine's <see cref="SpamTrainingView"/>. Carries the
/// display strings the DataTemplate binds plus the raw hex id the per-row Undo
/// dispatches with. A primitive-only public API keeps the row decoupled from the
/// UniFFI-internal <c>SpamTrainingView</c> / <c>TrainingLabel</c> / <c>TrainingSource</c>.
/// </summary>
public sealed class MailSpamTrainingRow
{
    /// <summary>Lowercase hex history id — the per-row Undo key.</summary>
    public required string HistoryIdHex { get; init; }
    /// <summary><c>mail-spam-training-history-list-item-message</c>.</summary>
    public required string Message { get; init; }
    /// <summary><c>mail-spam-training-history-list-item-label</c> (Spam / Not spam).</summary>
    public required string LabelBadge { get; init; }
    /// <summary><c>mail-spam-training-history-list-item-source</c> (signal source).</summary>
    public required string SourceBadge { get; init; }
    /// <summary><c>mail-spam-training-history-list-item-created-at</c> (local date).</summary>
    public required string CreatedAt { get; init; }
    public required string UndoLabel { get; init; }

    internal static MailSpamTrainingRow From(SpamTrainingView v) => new()
    {
        HistoryIdHex = v.historyIdHex,
        Message = v.message,
        LabelBadge = Strings.Resolve(FaunaClientMailSettingsMethods.TrainingLabelBadge(v.label)),
        SourceBadge = Strings.Resolve(FaunaClientMailSettingsMethods.TrainingSourceBadge(v.source)),
        CreatedAt = FormatMillisLocal(v.createdAtMs),
        UndoLabel = Strings.Get("mail_spam/undo"),
    };

    private static string FormatMillisLocal(long ms) =>
        uniffi.fauna_ffi.FaunaFfiMethods.FormatUnixLocalDateMs(ms);
}

/// <summary>
/// One <c>report-share-published-list-item</c> row — a ≥k report aggregate this
/// nest exports to peers (report-sharing.md § Client wire + transparency surface),
/// projected from an <see cref="FfiReportShareEntry"/>. Read-only; no dispatch.
/// </summary>
public sealed class ReportSharePublishedRow
{
    /// <summary><c>report-share-published-list-item-hash</c>.</summary>
    public required string ContentHash { get; init; }
    /// <summary><c>report-share-published-list-item-factor</c>.</summary>
    public required string Factor { get; init; }
    /// <summary><c>report-share-published-list-item-count</c>.</summary>
    public required string Count { get; init; }

    internal static ReportSharePublishedRow From(FfiReportShareEntry e) => new()
    {
        ContentHash = e.contentHash,
        Factor = e.factor,
        Count = e.count.ToString(),
    };
}

/// <summary>
/// The user-facing <c>mail-spam</c> page (docs/goal/behavior/mail-spam.md § Reset /
/// § Cold start Path 2 / § Training-sample retention + § Undo) — a person managing
/// their own per-account spam classifier: reset the per-user Bayesian model, opt
/// in/out of the deployment-baseline contribution, and review + undo individual
/// training events. A dumb projection over the shared
/// <c>fauna_client_mail_settings::MailSpamMachine</c>, consumed through its
/// UniFFI-generated <see cref="IMailSpamMachine"/> interface (machine-as-seam — no
/// hand-written seam; the page builds the real machine, the unit test fakes the
/// interface). All projection / action sequencing lives in shared Rust (priority #2);
/// this VM forwards the four user actions and re-projects the snapshot after each.
/// Mirrors <see cref="MailAliasesViewModel"/>; lifts the linux reference
/// apps/fauna-linux/src/settings/mail_spam.rs.
///
/// UI precedes backend, surfaced never faked (mail-spam.md § Implementation status
/// today): the per-user Bayesian feedback loop is unbuilt, so on a real nest every
/// action surfaces the seam's honest <c>unimplemented</c> rejection via
/// <c>error-message</c> and the training-history list stays empty (no fabricated rows).
///
/// <para>The distributed report-sharing opt-in + transparency list
/// (report-sharing.md § Client wire + transparency surface) rides alongside on the
/// same page but is <b>not</b> part of the machine — it's a plain bool + read-only
/// list, not a state machine — so it's a small dedicated read/write directly over
/// <see cref="INestRpcClient"/> (<c>fauna.moderation.report_share.{set,status}</c>),
/// mirroring linux's <c>ModerationClient</c> flow in <c>mail_spam.rs</c>.</para>
/// </summary>
public partial class MailSpamViewModel : ObservableObject
{
    private readonly IMailSpamMachine _machine;
    private readonly INestRpcClient _rpc;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    /// <summary><c>mail-spam-contribute-baseline-toggle</c> state (default off,
    /// user-controls-their-data). Reflects the persisted flag after each load / action.</summary>
    [ObservableProperty] private bool _contributeBaseline;

    /// <summary><c>mail-spam-share-reports-toggle</c> state (default off,
    /// user-controls-their-data; report-sharing.md § Client wire). Reflects the
    /// persisted opt-in after each load / toggle.</summary>
    [ObservableProperty] private bool _shareReports;

    /// <summary>The training-history rows (one <c>mail-spam-training-history-list</c> row
    /// each), rebuilt from the snapshot on every projection.</summary>
    public ObservableCollection<MailSpamTrainingRow> Events { get; } = new();

    /// <summary>The <c>report-share-published-list</c> rows — the ≥k report
    /// aggregates this nest exports to peers, rebuilt from
    /// <c>fauna.moderation.report_share.status</c> on page-load and after every
    /// toggle.</summary>
    public ObservableCollection<ReportSharePublishedRow> PublishedReports { get; } = new();

    /// <summary><c>mail-spam-threshold-override-input</c> text (mail-policy-config.md
    /// § Tier 3). Empty means the account follows the admin default; <c>"0"</c> is a
    /// real setting, never collapsed into empty. Reflects the persisted value after
    /// each load / commit, never the local keystroke.</summary>
    [ObservableProperty] private string _thresholdOverrideText = string.Empty;

    internal MailSpamViewModel(IMailSpamMachine machine, INestRpcClient rpc)
    {
        _machine = machine;
        _rpc = rpc;
    }

    /// <summary>Initial page load: hydrate the training history + contribution flag,
    /// then project the snapshot. The transport already tolerates the post-login connect
    /// race for a single RPC (transport.md § Request lifecycle step 3) — no app-level
    /// retry needed here.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            await _machine.Hydrate();
            Apply(_machine.Snapshot());
            await LoadReportShareAsync();
            await LoadThresholdOverrideAsync();
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Read the report-share opt-in + published list
    /// (<c>fauna.moderation.report_share.status</c>) and apply it. The transport already
    /// tolerates the post-login connect race for a single RPC (transport.md § Request
    /// lifecycle step 3) — no app-level retry needed here. A failure propagates to
    /// <see cref="LoadAsync"/>'s catch.</summary>
    private async Task LoadReportShareAsync()
    {
        ApplyReportShare(await _rpc.ModerationReportShareStatusAsync());
    }

    /// <summary>Reset the per-user classifier — delete the model + all training history.
    /// Irreversible (the page confirms first).</summary>
    public Task ResetModelAsync() => DispatchAsync(new MailSpamAction.ResetModel());

    /// <summary>Opt the account's training in/out of the deployment baseline.</summary>
    public Task SetContributeBaselineAsync(bool contribute)
        => DispatchAsync(new MailSpamAction.SetContributeBaseline(contribute));

    /// <summary>Set the report-share opt-in (<c>fauna.moderation.report_share.set</c>),
    /// then re-read status so the toggle + published list reflect the persisted value
    /// — opting out withdraws this actor's contributed reports, which may shrink the
    /// list (report-sharing.md § Client wire). Mirrors linux's set_report_share.</summary>
    public async Task SetShareReportsAsync(bool share)
    {
        try
        {
            await _rpc.ModerationReportShareSetAsync(share);
            ApplyReportShare(await _rpc.ModerationReportShareStatusAsync());
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
        }
    }

    /// <summary>Undo one training event by its hex id (applies the inverse n-gram delta +
    /// deletes the row). Reversible-by-retrain, so no confirm.</summary>
    public Task UndoTrainingAsync(string historyIdHex)
        => DispatchAsync(new MailSpamAction.UndoTraining(historyIdHex));

    /// <summary>Read <c>fauna.bridges.get_spam_threshold_override</c> and reflect it into
    /// <see cref="ThresholdOverrideText"/> (<c>null</c> renders empty; <c>0</c> renders
    /// "0", never blank). A single NestClient RPC — the transport already tolerates the
    /// post-login connect race (transport.md § Request lifecycle step 3). A failure
    /// propagates to <see cref="LoadAsync"/>'s catch.</summary>
    private async Task LoadThresholdOverrideAsync()
        => ApplyThresholdOverride(await _rpc.SpamThresholdOverrideGetAsync());

    /// <summary>Parse <see cref="ThresholdOverrideText"/>'s CURRENT value (empty → clear the
    /// override; <c>fauna_ffi::ParseCount</c>, the same validator the alias add-sheet's own
    /// spam-threshold field uses — <c>Some(0)</c> is a real setting, never collapsed into
    /// "unset"), set it (<c>fauna.bridges.set_spam_threshold_override</c>), then reflect the
    /// PERSISTED value — never the local keystroke, the <see cref="SetShareReportsAsync"/>
    /// shape. Called on both a commit-on-Enter and a blur (the input has no separate save
    /// button, tui's/linux's shape).</summary>
    public async Task CommitThresholdOverrideAsync()
    {
        try
        {
            var value = FaunaFfiMethods.ParseCount(ThresholdOverrideText);
            ApplyThresholdOverride(await _rpc.SpamThresholdOverrideSetAsync(value));
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
        }
    }

    private void ApplyThresholdOverride(uint? value)
        => ThresholdOverrideText = value?.ToString() ?? string.Empty;

    /// <summary>Dispatch an action then re-project the snapshot. The machine captures any
    /// user-facing error into <c>snapshot.error</c> (and also throws), so the throw is
    /// swallowed and the error read from the snapshot — matching MailAliasesViewModel /
    /// linux; the exception is a fallback only if the snapshot carried no error.</summary>
    private async Task DispatchAsync(MailSpamAction action)
    {
        try
        {
            await _machine.Dispatch(action);
            Apply(_machine.Snapshot());
        }
        catch (Exception ex)
        {
            Apply(_machine.Snapshot());
            if (string.IsNullOrEmpty(Error)) Error = Strings.Error(ex);
        }
    }

    private void Apply(MailSpamSnapshot snap)
    {
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        ContributeBaseline = snap.contributeBaseline;
        Events.Clear();
        foreach (var e in snap.events)
        {
            Events.Add(MailSpamTrainingRow.From(e));
        }
    }

    private void ApplyReportShare(FfiReportShareStatus status)
    {
        ShareReports = status.share;
        PublishedReports.Clear();
        foreach (var e in status.published)
        {
            PublishedReports.Add(ReportSharePublishedRow.From(e));
        }
    }
}
