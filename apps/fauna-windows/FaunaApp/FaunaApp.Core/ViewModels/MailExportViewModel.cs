using System;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_mail;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>Which wizard screen is showing — the public mirror of the UniFFI-internal
/// <c>ExportStep</c>, so the panel can gate group visibility without referencing the
/// internal enum (same decoupling as <see cref="MailAliasRow"/>'s <c>bool IsWildcard</c>).</summary>
public enum MailExportStep { Format, Scope, Confirm, Progress, Done }

/// <summary>One source-mailbox option in the scope step's multi-select
/// (<c>mail-export-scope-mailboxes</c>), projected from the shared machine's
/// <c>MailboxOption</c>.</summary>
public sealed class MailExportMailboxRow
{
    public required string Name { get; init; }
    public required bool Selected { get; init; }
    /// <summary>The row's <c>state</c> attribute, read by the e2e walk through
    /// <c>AutomationProperties.HelpText</c> — the cross-app contract every app serves
    /// (<c>actions/mail_export.py::mailbox_selected</c> reads this rather than the
    /// checkbox glyph, which is a per-app rendering detail). Mirrors
    /// <c>MailImportMailboxRow.State</c>.</summary>
    public string State => Selected ? "on" : "off";
}

/// <summary>One per-mailbox progress row (<c>mail-export-mailbox-progress-list-item</c>),
/// projected from the shared machine's <c>MailboxProgressView</c>.</summary>
public sealed class MailExportMailboxProgressRow
{
    public required string Name { get; init; }
    /// <summary><c>…-list-item-progress</c> — "exported/total".</summary>
    public required string Progress { get; init; }
}

/// <summary>
/// The user-facing <c>mail-export</c> wizard (docs/goal/behavior/mail-export.md § UX
/// shape): a five-step wizard (Format → Scope → Confirm → Progress → Done) that pulls
/// the user's mail-area state out in one of three MUA-portable formats. A dumb
/// projection over the shared <c>fauna_client_mail_settings::MailExportMachine</c>,
/// consumed through its UniFFI-generated <see cref="IMailExportMachine"/> interface
/// (machine-as-seam — no hand-written seam). The wizard FSM (step transitions, format
/// pick, scope default-selection, start/pause/resume/cancel sequencing) lives in shared
/// Rust (priority #2); this VM forwards actions and re-projects the snapshot after each.
/// Mirrors <see cref="MailAliasesViewModel"/>; lifts apps/fauna-linux/src/settings/mail_export.rs.
///
/// <para><b>This VM drives the export</b> (mail-export.md § Implementation status
/// today — tui's, linux's and FaunaKit's <c>MailExportVM</c> leg, lifted). The machine
/// is built with key custody (<c>INestRpcClient.BuildMailExportMachineAsync</c>), so the
/// VM does the three things custody obliges; custody and the spawn are one change,
/// since custody alone would open a session nothing drives:</para>
/// <para>1. After a <c>Start</c>/<c>Resume</c> whose <i>post-dispatch</i> snapshot reads
/// <c>Running</c> — never a rejected one — <see cref="ShouldDriveExport"/> goes true and
/// the panel fires <see cref="DriveExportAsync"/> (the shared machine never self-spawns
/// <c>run_export</c>), exactly as <see cref="MailImportViewModel"/> drives
/// <c>run_import</c>.</para>
/// <para>2. The panel repaints Progress on a tick through <see cref="Repaint"/>, which
/// only re-reads the snapshot — it touches no panel state, so it cannot disarm an armed
/// Cancel.</para>
/// <para>3. Download runs § Download flow (<c>MailExportAction.Download</c>) into the
/// save directory the machine was built with, and the snapshot's
/// <c>savedArchivePath</c> names where it went — the Done summary says so
/// (<c>mail_export.saved_summary_fmt</c>), so the press is visible.</para>
/// <para>The actor handle names the archive's root directory and saved file; it may
/// arrive after the machine is built or change with the user, so it is read at the
/// gesture (<c>SetActorHandle</c> before Start / Resume / Download), never only at
/// construction.</para>
/// </summary>
public partial class MailExportViewModel : ObservableObject
{
    private readonly IMailExportMachine _machine;
    /// <summary>Reads the signed-in account's current handle (empty when not yet known).</summary>
    private readonly Func<string> _currentHandle;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    [ObservableProperty] private MailExportStep _step;
    /// <summary>Format-picker selection: 0 = mbox, 1 = Maildir++, 2 = EML-zip.</summary>
    [ObservableProperty] private int _formatIndex;

    // ── Scope (step 2) ──
    [ObservableProperty] private string _dateFrom = string.Empty;
    [ObservableProperty] private string _dateTo = string.Empty;
    [ObservableProperty] private bool _stripHeaders;

    // ── Confirm / Progress / Done summaries (presentation glue, matching the linux lead) ──
    [ObservableProperty] private string _confirmSummary = string.Empty;
    [ObservableProperty] private string _progressSummary = string.Empty;
    [ObservableProperty] private double _progressFraction;
    [ObservableProperty] private string _errorLog = string.Empty;
    [ObservableProperty] private string _doneSummary = string.Empty;
    [ObservableProperty] private string _downloadUrl = string.Empty;
    /// <summary>Pause is offered only while the session is running.</summary>
    [ObservableProperty] private bool _canPause;
    /// <summary>Resume is offered only while the session is paused.</summary>
    [ObservableProperty] private bool _canResume;

    /// <summary>True once a <c>Start</c>/<c>Resume</c> came back with the session really
    /// <c>Running</c>; the panel reads it to spawn <see cref="DriveExportAsync"/> exactly
    /// once per accepted transition. Cleared by the drive loop's own completion.</summary>
    public bool ShouldDriveExport { get; private set; }

    /// <summary>The scope step's source mailboxes (one <c>mail-export-scope-mailboxes</c>
    /// checkbox each), rebuilt from the snapshot on every projection.</summary>
    public ObservableCollection<MailExportMailboxRow> Mailboxes { get; } = new();

    /// <summary>The progress step's per-mailbox rows
    /// (<c>mail-export-mailbox-progress-list</c>), rebuilt on every projection.</summary>
    public ObservableCollection<MailExportMailboxProgressRow> MailboxProgress { get; } = new();

    internal MailExportViewModel(IMailExportMachine machine, Func<string>? currentHandle = null)
    {
        _machine = machine;
        _currentHandle = currentHandle ?? (() => string.Empty);
    }

    /// <summary>Initial page load: hydrate the mailbox options + storage mode (and resume
    /// any active session), then project the snapshot. The transport already tolerates
    /// the post-login connect race for a single RPC (transport.md § Request lifecycle
    /// step 3) — no app-level retry needed here.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            await _machine.Hydrate();
            Apply(_machine.Snapshot());
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

    // ── Step 1 — Format ──
    public Task SelectFormatAsync(int index)
        => DispatchAsync(new MailExportAction.SelectFormat(FormatAt(index)));

    // ── Step 2 — Scope ──
    public Task ToggleMailboxAsync(string mailbox)
        => DispatchAsync(new MailExportAction.ToggleMailbox(mailbox));
    public Task SetDateFromAsync(string value)
        => DispatchAsync(new MailExportAction.SetDateFrom(value));
    public Task SetDateToAsync(string value)
        => DispatchAsync(new MailExportAction.SetDateTo(value));
    public Task SetStripHeadersAsync(bool on)
        => DispatchAsync(new MailExportAction.SetStripHeaders(on));

    // ── Navigation + lifecycle ──
    public Task NextAsync() => DispatchAsync(new MailExportAction.Next());
    public Task BackAsync() => DispatchAsync(new MailExportAction.Back());
    /// <summary>Durable commit — opens the <c>export_sessions</c> row. Arms
    /// <see cref="ShouldDriveExport"/> only when the session really came back
    /// <c>Running</c>.</summary>
    public Task StartAsync() => DispatchAndMaybeDriveAsync(new MailExportAction.Start());
    public Task PauseAsync() => DispatchAsync(new MailExportAction.Pause());
    public Task ResumeAsync() => DispatchAndMaybeDriveAsync(new MailExportAction.Resume());
    public Task CancelAsync() => DispatchAsync(new MailExportAction.Cancel());
    public Task DiscardAsync() => DispatchAsync(new MailExportAction.Discard());
    public Task RefreshAsync() => DispatchAsync(new MailExportAction.Refresh());

    /// <summary>The Done-step <c>mail-export-download-button</c>: § Download flow —
    /// fetch, open and save the archive into the machine's save directory. A refused
    /// (truncated or unterminated) archive lands on <c>error-message</c> and leaves
    /// nothing in that directory (the shared sink's <c>.part</c>-then-rename).</summary>
    public Task DownloadAsync()
    {
        PushHandle();
        return DispatchAsync(new MailExportAction.Download());
    }

    /// <summary>The export drive loop. Returns when the session completes, when
    /// Pause/Cancel was dispatched concurrently, or on a session-fatal error — whichever
    /// comes first. Awaited WITHOUT <c>ConfigureAwait(false)</c> on purpose: the
    /// continuation must land back on the UI thread, since it repaints the two bound
    /// ObservableCollections (a WinUI VM that configures the context away throws a silent
    /// COMException).</summary>
    public async Task DriveExportAsync()
    {
        string? loopError = null;
        try
        {
            await _machine.RunExport();
        }
        catch (Exception ex)
        {
            loopError = Strings.Error(ex);
        }
        finally
        {
            ShouldDriveExport = false;
            Apply(_machine.Snapshot());
            // A failure normally lands on the snapshot's own error too; keep the loop's
            // exception only when the snapshot has nothing to say.
            if (string.IsNullOrEmpty(Error)) Error = loopError;
        }
    }

    /// <summary>Re-project the machine's current snapshot with no dispatch — the repaint
    /// tick the panel runs while <see cref="DriveExportAsync"/> mutates the machine in
    /// the background.</summary>
    public void Repaint() => Apply(_machine.Snapshot());

    private async Task DispatchAndMaybeDriveAsync(MailExportAction action)
    {
        PushHandle();
        await DispatchAsync(action);
        // Only an ACCEPTED transition arms the loop: a rejected Start leaves the session
        // untouched (or absent), and spawning run_export on it would drive nothing while
        // hiding the rejection behind a Progress screen.
        if (_machine.Snapshot().sessionState == ExportSessionState.Running) ShouldDriveExport = true;
    }

    /// <summary>Refresh the machine's actor handle from the session, read now. An empty
    /// one (not yet known on a fresh sign-in) is never pushed — the machine keeps what it
    /// was built with rather than naming the archive after nobody.</summary>
    private void PushHandle()
    {
        var handle = _currentHandle();
        if (!string.IsNullOrEmpty(handle)) _machine.SetActorHandle(handle);
    }

    private async Task DispatchAsync(MailExportAction action)
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

    private void Apply(MailExportSnapshot snap)
    {
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        Step = StepOf(snap.step);
        FormatIndex = IndexOf(snap.format);
        DateFrom = snap.dateFrom;
        DateTo = snap.dateTo;
        StripHeaders = snap.stripHeaders;

        Mailboxes.Clear();
        foreach (var m in snap.mailboxes)
        {
            Mailboxes.Add(new MailExportMailboxRow { Name = m.name, Selected = m.selected });
        }

        var selectedCount = snap.mailboxes.Count(m => m.selected);
        ConfirmSummary = Strings.Format("mail_export/confirm_summary_fmt", FormatLabel(snap.format), selectedCount);
        ProgressSummary = Strings.Format(
            "mail_export/progress_summary_fmt",
            snap.exportedCount, snap.totalCount, snap.skippedCount, snap.erroredCount);
        ProgressFraction = FaunaFfiMethods.QuotaFraction(snap.exportedCount, snap.totalCount);
        ErrorLog = string.Join("\n", snap.errorLog);
        // Once the archive is on disk the summary says WHERE — the visible answer to the
        // Download press (linux's done_text, tui's done_step_elements).
        DoneSummary = snap.blobBytes is ulong b
            ? string.IsNullOrEmpty(snap.savedArchivePath)
                ? Strings.Format("mail_export/done_summary_fmt", FormatLabel(snap.format), b)
                : Strings.Format("mail_export/saved_summary_fmt", FormatLabel(snap.format), b, snap.savedArchivePath)
            : FormatLabel(snap.format);
        DownloadUrl = snap.downloadUrl;
        CanPause = snap.sessionState == ExportSessionState.Running;
        CanResume = snap.sessionState == ExportSessionState.Paused;

        MailboxProgress.Clear();
        foreach (var mp in snap.mailboxProgress)
        {
            MailboxProgress.Add(new MailExportMailboxProgressRow
            {
                Name = mp.name,
                Progress = $"{mp.exported}/{mp.total}",
            });
        }
    }

    private static MailExportStep StepOf(ExportStep s) => s switch
    {
        ExportStep.Format => MailExportStep.Format,
        ExportStep.Scope => MailExportStep.Scope,
        ExportStep.Confirm => MailExportStep.Confirm,
        ExportStep.Progress => MailExportStep.Progress,
        ExportStep.Done => MailExportStep.Done,
        _ => MailExportStep.Format,
    };

    private static int IndexOf(ExportFormat f) => f switch
    {
        ExportFormat.Mbox => 0,
        ExportFormat.MaildirPlus => 1,
        ExportFormat.EmlZip => 2,
        _ => 0,
    };

    private static ExportFormat FormatAt(int index) => index switch
    {
        1 => ExportFormat.MaildirPlus,
        2 => ExportFormat.EmlZip,
        _ => ExportFormat.Mbox,
    };

    private static string FormatLabel(ExportFormat f) =>
        Strings.Resolve(FaunaClientMailSettingsMethods.ExportFormatLabel(f));
}
