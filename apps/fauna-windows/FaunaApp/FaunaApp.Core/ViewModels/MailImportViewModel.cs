using System;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>Which wizard screen is showing — the public mirror of the UniFFI-internal
/// <c>ImportStep</c>, so the panel can gate group visibility without referencing the
/// internal enum (the <see cref="MailExportStep"/> precedent).</summary>
public enum MailImportStep { Source, Scope, Confirm, Progress, Done }

/// <summary>Which Source-step fields the picked provider shows
/// (<c>mailbox-migration.md</c> § Wizard steps step 1's "Required fields" table, as
/// transcribed by the tui lead app). Gmail/iCloud take an app password; Outlook shows
/// the OAuth button <b>and</b> the IMAP-fallback fields; Generic shows the IMAP fields
/// alone. A pure function of the pick, so the panel can apply it inside the picker's own
/// handler without waiting for a dispatch to come back.</summary>
public sealed record MailImportSourceFields(
    bool ShowAppPassword,
    bool ShowOauthButton,
    bool ShowImapFields);

/// <summary>One source-mailbox row on the Scope step
/// (indexed <c>mail-import-scope-mailbox-item</c>), projected from the shared machine's
/// <c>SourceMailboxOption</c>.</summary>
public sealed class MailImportMailboxRow
{
    public required string Name { get; init; }
    public required bool Selected { get; init; }
    /// <summary>The row's <c>state</c> attribute, read by the e2e walk through
    /// <c>AutomationProperties.HelpText</c> — the cross-app contract every app serves
    /// (<c>actions/mail_import.py::mailbox_selected</c> reads this rather than the
    /// checkbox glyph, which is a per-app rendering detail).</summary>
    public string State => Selected ? "on" : "off";
}

/// <summary>One per-mailbox progress row
/// (<c>mail-import-mailbox-progress-list-item</c>).</summary>
public sealed class MailImportMailboxProgressRow
{
    public required string Name { get; init; }
    /// <summary><c>…-list-item-progress</c> — the mailbox's planned message count.</summary>
    public required string Progress { get; init; }
}

/// <summary>
/// The user-facing <c>mail-import</c> wizard (docs/goal/behavior/mailbox-migration.md
/// § UX shape / § Wizard steps / § Credential handling): a five-screen wizard
/// (Source → Scope → Confirm → Progress → Done) that pulls a person's existing mail off
/// a foreign IMAP server and into their own nest. A dumb projection over the shared
/// <c>fauna_client_mail_settings::MailImportMachine</c>, consumed through its
/// UniFFI-generated <see cref="IMailImportMachine"/> interface (machine-as-seam — no
/// hand-written seam). The wizard FSM lives in shared Rust (priority #2); this VM
/// forwards actions and re-projects the snapshot after each.
/// Mirrors <see cref="MailExportViewModel"/>; lifts apps/fauna-linux/src/settings/mail_import.rs.
///
/// <para><b>Unlike the export twin, the backend is REAL.</b> Every import RPC has
/// shipped since 2026-07-08 and both machine seams are real, so Connect/Start/Pause/
/// Resume/Cancel drive a genuine <c>import_sessions</c> row against a genuine foreign
/// IMAP source. A rejection is a real answer bridged onto <c>error-message</c> — never
/// a fake-green.</para>
///
/// <para><b>This VM spawns the fetch-drive loop itself.</b> The shared machine
/// deliberately does not self-spawn <c>run_import</c> (goal doc § Implementation status
/// today): after a <c>Start</c>/<c>Resume</c> whose post-dispatch snapshot really
/// reports <c>Running</c> — never a rejected one — <see cref="ShouldDriveImport"/> goes
/// true and the panel fires <see cref="DriveImportAsync"/> plus a repaint ticker.
/// Skip it and the Progress screen sits at zero while the session is genuinely open,
/// which reads exactly like a nest bug.</para>
///
/// <para><b>The Source/Scope text fields are page-local drafts.</b> The panel's own
/// TextBoxes are the buffers, committed as ONE ordered multi-action dispatch at
/// Connect/Next through the shared <c>ConnectActions</c>/<c>ScopeNextActions</c>
/// sequences — not a <c>Set*</c> per keystroke. A per-keystroke dispatch spawns one task
/// per character and lets the Connect click be ordered before the last one, which on
/// this page means logging into the source with a truncated password.</para>
/// </summary>
public partial class MailImportViewModel : ObservableObject
{
    private readonly IMailImportMachine _machine;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    [ObservableProperty] private MailImportStep _step;
    /// <summary>Source-picker selection: 0 = Gmail, 1 = Outlook, 2 = iCloud, 3 = Generic.</summary>
    [ObservableProperty] private int _sourceKindIndex;
    /// <summary>TLS-mode picker selection: 0 = implicit, 1 = STARTTLS.</summary>
    [ObservableProperty] private int _tlsModeIndex;

    /// <summary>The snapshot's own <c>host</c>/<c>port</c> — NOT the panel's draft
    /// TextBoxes, which never bind to these. Refreshed on every projection
    /// (<see cref="Apply"/>) so they are always current, but nothing reads them except
    /// the Source picker's own change handler right after a kind change — reading them
    /// anywhere in the general render path would re-seed the drafts on every dispatch,
    /// including a rejected Connect, clobbering what the user typed.</summary>
    public string Host { get; private set; } = string.Empty;
    public string Port { get; private set; } = string.Empty;

    // ── Confirm / Progress / Done summaries (presentation glue, matching the linux lead) ──
    [ObservableProperty] private string _confirmSummary = string.Empty;
    [ObservableProperty] private string _progressSummary = string.Empty;
    [ObservableProperty] private double _progressFraction;
    [ObservableProperty] private string _errorLog = string.Empty;
    [ObservableProperty] private string _doneSummary = string.Empty;
    /// <summary>Pause is offered only while the session is running.</summary>
    [ObservableProperty] private bool _canPause;
    /// <summary>Resume is offered only while the session is paused.</summary>
    [ObservableProperty] private bool _canResume;

    /// <summary>True once a <c>Start</c>/<c>Resume</c> came back with the session really
    /// <c>Running</c>; the panel reads it to spawn <see cref="DriveImportAsync"/> exactly
    /// once per accepted transition. Cleared by the drive loop's own completion.</summary>
    public bool ShouldDriveImport { get; private set; }

    /// <summary>The Scope step's source mailboxes, rebuilt from the snapshot on every
    /// projection.</summary>
    public ObservableCollection<MailImportMailboxRow> Mailboxes { get; } = new();

    /// <summary>The Progress step's per-mailbox rows
    /// (<c>mail-import-mailbox-progress-list</c>), rebuilt on every projection.
    /// The machine tracks only GLOBAL imported/skipped/errored counts, not a per-mailbox
    /// breakdown (<c>MailImportSnapshot</c> has no <c>mailboxProgress</c> field the way
    /// export's does) — so each row shows its planned message count, not a live
    /// per-mailbox fraction. The tui lead app's own accurate-to-what-exists
    /// simplification, not a gap this page adds.</summary>
    public ObservableCollection<MailImportMailboxProgressRow> MailboxProgress { get; } = new();

    internal MailImportViewModel(IMailImportMachine machine)
    {
        _machine = machine;
    }

    /// <summary>Initial page load: hydrate any active session and jump to its
    /// Progress/Done screen, then project the snapshot. The transport already
    /// tolerates the post-login connect race for a single RPC (transport.md
    /// § Request lifecycle step 3) — no app-level retry needed here.</summary>
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

    // ── Step 1 — Source ──

    /// <summary>Pick the provider. Pre-fills host/port/tls_mode for a preset kind — the
    /// machine's own <c>SelectSourceKind</c> arm does that (shared Rust, never re-derived
    /// here), and <see cref="Host"/>/<see cref="Port"/> below carry the result out so the
    /// panel's picker handler can re-seed its own draft TextBoxes from it — this
    /// comment used to claim the panel already did that, and it did not: the host field
    /// went out blank on Outlook and <c>Connect</c> sent it verbatim.</summary>
    public Task SelectSourceKindAsync(int index)
        => DispatchAsync(new MailImportAction.SelectSourceKind(KindAt(index)));

    public Task SetTlsModeAsync(int index)
        => DispatchAsync(new MailImportAction.SetTlsMode(TlsModeAt(index)));

    /// <summary>Step 1→3 — the whole Source form as ONE ordered multi-action dispatch,
    /// sequenced by the shared <c>ConnectActions</c> so windows does not hand-roll a
    /// copy of the ordering (priority #2/#4). The caller stays responsible for locating
    /// its own field values, which is exactly the division of labor that shared function
    /// documents.</summary>
    public async Task ConnectAsync(string host, string port, string username, string password)
    {
        var actions = FaunaClientMailSettingsMethods.ConnectActions(
            KindAt(SourceKindIndex), host, port, username, password);
        foreach (var action in actions)
        {
            if (!await DispatchOneAsync(action)) break;
        }
        Apply(_machine.Snapshot());
    }

    // ── Step 3 — Scope ──
    public Task ToggleMailboxAsync(string mailbox)
        => DispatchAsync(new MailImportAction.ToggleMailbox(mailbox));

    /// <summary>Scope → Confirm — both drafts committed then the advance, sequenced by
    /// the shared <c>ScopeNextActions</c> (an unparseable max-size buffer falls back to
    /// the machine's own 50 MiB default rather than sending a stale value).</summary>
    public async Task ScopeNextAsync(string dateFrom, string maxSizeMb)
    {
        var actions = FaunaClientMailSettingsMethods.ScopeNextActions(dateFrom, maxSizeMb);
        foreach (var action in actions)
        {
            if (!await DispatchOneAsync(action)) break;
        }
        Apply(_machine.Snapshot());
    }

    // ── Navigation + lifecycle ──
    public Task BackAsync() => DispatchAsync(new MailImportAction.Back());
    public Task RefreshAsync() => DispatchAsync(new MailImportAction.Refresh());

    /// <summary>Durable commit — opens the <c>import_sessions</c> row. Arms
    /// <see cref="ShouldDriveImport"/> only when the session really came back
    /// <c>Running</c>.</summary>
    public Task StartAsync() => DispatchAndMaybeDriveAsync(new MailImportAction.Start());
    public Task ResumeAsync() => DispatchAndMaybeDriveAsync(new MailImportAction.Resume());
    public Task PauseAsync() => DispatchAsync(new MailImportAction.Pause());
    public Task CancelAsync() => DispatchAsync(new MailImportAction.Cancel());

    /// <summary>The fetch-drive loop. Returns when every selected mailbox is exhausted,
    /// when Pause/Cancel was dispatched concurrently, or on a session-fatal error —
    /// whichever comes first. Awaited WITHOUT <c>ConfigureAwait(false)</c> on purpose:
    /// the continuation must land back on the UI thread, since it repaints the two bound
    /// ObservableCollections (a WinUI VM that configures the context away throws a silent
    /// COMException).</summary>
    public async Task DriveImportAsync()
    {
        try
        {
            await _machine.RunImport();
        }
        catch (Exception ex)
        {
            if (string.IsNullOrEmpty(Error)) Error = Strings.Error(ex);
        }
        finally
        {
            ShouldDriveImport = false;
            Apply(_machine.Snapshot());
        }
    }

    /// <summary>Re-project the machine's current snapshot with no dispatch — the repaint
    /// tick the panel runs while <see cref="DriveImportAsync"/> mutates the machine in
    /// the background.</summary>
    public void Repaint() => Apply(_machine.Snapshot());

    private async Task DispatchAndMaybeDriveAsync(MailImportAction action)
    {
        await DispatchOneAsync(action);
        var snap = _machine.Snapshot();
        Apply(snap);
        // Only an ACCEPTED transition arms the loop: a rejected Start leaves the session
        // untouched (or absent), and spawning run_import on it would drive nothing while
        // hiding the rejection behind a spinner.
        if (snap.sessionState == ImportSessionState.Running) ShouldDriveImport = true;
    }

    private async Task DispatchAsync(MailImportAction action)
    {
        await DispatchOneAsync(action);
        Apply(_machine.Snapshot());
    }

    /// <summary>Dispatch one action; false when it threw, so an ordered multi-action
    /// sequence stops at the first rejection instead of pressing on with a half-applied
    /// form.</summary>
    private async Task<bool> DispatchOneAsync(MailImportAction action)
    {
        try
        {
            await _machine.Dispatch(action);
            return true;
        }
        catch (Exception ex)
        {
            Apply(_machine.Snapshot());
            if (string.IsNullOrEmpty(Error)) Error = Strings.Error(ex);
            return false;
        }
    }

    private void Apply(MailImportSnapshot snap)
    {
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        Step = StepOf(snap.step);
        SourceKindIndex = IndexOf(snap.sourceKind);
        TlsModeIndex = snap.tlsMode == ImportTlsMode.StartTls ? 1 : 0;
        Host = snap.host;
        Port = snap.port.ToString();

        Mailboxes.Clear();
        foreach (var m in snap.mailboxes)
        {
            Mailboxes.Add(new MailImportMailboxRow { Name = m.name, Selected = m.selected });
        }

        var selected = snap.mailboxes.Where(m => m.selected).ToList();
        ulong plannedMessages = 0;
        foreach (var m in selected) plannedMessages += m.messageCount;

        ConfirmSummary = Strings.Format(
            "mail_import/confirm_summary_fmt",
            SourceKindLabel(snap.sourceKind), selected.Count, plannedMessages);
        ProgressSummary = Strings.Format(
            "mail_import/progress_summary_fmt",
            snap.importedCount, snap.totalCount, snap.skippedCount, snap.erroredCount);
        ProgressFraction = FaunaFfiMethods.QuotaFraction((long)snap.importedCount, (long)snap.totalCount);
        ErrorLog = string.Join("\n", snap.errorLog);
        DoneSummary = Strings.Format(
            "mail_import/done_summary_fmt",
            snap.importedCount, snap.skippedCount, snap.erroredCount);
        CanPause = snap.sessionState == ImportSessionState.Running;
        CanResume = snap.sessionState == ImportSessionState.Paused;

        MailboxProgress.Clear();
        foreach (var m in selected)
        {
            MailboxProgress.Add(new MailImportMailboxProgressRow
            {
                Name = m.name,
                Progress = Strings.Format("mail_import/progress_row_fmt", m.messageCount),
            });
        }
    }

    /// <summary>Which Source-step fields the picked provider shows. Public + static so
    /// the panel can apply it synchronously inside the picker's own change handler: the
    /// e2e walk picks a provider and types into the field it reveals with NO wait in
    /// between, and a hidden field is absent from the UIA tree, so leaving this to the
    /// dispatch's async round trip makes the very next keystroke 404.</summary>
    public static MailImportSourceFields FieldsFor(int sourceKindIndex) => KindAt(sourceKindIndex) switch
    {
        // Outlook shows the OAuth button AND the IMAP-fallback fields — ui.yaml's own
        // "(+ Outlook fallback)" annotation on host/port/tls-mode/username/password is
        // what settles this, and the fallback is the only working path today.
        ImportSourceKind.Outlook => new MailImportSourceFields(false, true, true),
        ImportSourceKind.Generic => new MailImportSourceFields(false, false, true),
        // Gmail / iCloud: app password only; host/port/tls-mode stay on the preset the
        // machine's SelectSourceKind arm applied.
        _ => new MailImportSourceFields(true, false, false),
    };

    private static MailImportStep StepOf(ImportStep s) => s switch
    {
        ImportStep.Source => MailImportStep.Source,
        ImportStep.Scope => MailImportStep.Scope,
        ImportStep.Confirm => MailImportStep.Confirm,
        ImportStep.Progress => MailImportStep.Progress,
        ImportStep.Done => MailImportStep.Done,
        _ => MailImportStep.Source,
    };

    private static int IndexOf(ImportSourceKind k) => k switch
    {
        ImportSourceKind.Gmail => 0,
        ImportSourceKind.Outlook => 1,
        ImportSourceKind.ICloud => 2,
        ImportSourceKind.Generic => 3,
        _ => 0,
    };

    private static ImportSourceKind KindAt(int index) => index switch
    {
        1 => ImportSourceKind.Outlook,
        2 => ImportSourceKind.ICloud,
        3 => ImportSourceKind.Generic,
        _ => ImportSourceKind.Gmail,
    };

    private static ImportTlsMode TlsModeAt(int index) =>
        index == 1 ? ImportTlsMode.StartTls : ImportTlsMode.Implicit;

    /// <summary>The provider's display label, resolved through the shared
    /// <c>ImportSourceKindLabel</c> — the <c>ExportFormatLabel</c> precedent. Windows
    /// hand-rolls neither picker vocabulary. <c>internal</c>, not public: the generated
    /// UniFFI enums are internal to this assembly, so a public signature over one is a
    /// CS0051 accessibility error. The panel consumes <see cref="SourceKindLabels"/>
    /// instead, which hands back plain strings.</summary>
    internal static string SourceKindLabel(ImportSourceKind k) =>
        Strings.Resolve(FaunaClientMailSettingsMethods.ImportSourceKindLabel(k));

    /// <summary>The TLS mode's display label, resolved the same shared way.</summary>
    internal static string TlsModeLabel(ImportTlsMode m) =>
        Strings.Resolve(FaunaClientMailSettingsMethods.ImportTlsModeLabel(m));

    /// <summary>The four provider labels in picker order, for the panel's
    /// <c>mail-import-source-picker</c> ItemsSource.</summary>
    public static string[] SourceKindLabels() => new[]
    {
        SourceKindLabel(ImportSourceKind.Gmail),
        SourceKindLabel(ImportSourceKind.Outlook),
        SourceKindLabel(ImportSourceKind.ICloud),
        SourceKindLabel(ImportSourceKind.Generic),
    };

    /// <summary>The two TLS-mode labels in picker order, for
    /// <c>mail-import-source-tls-mode</c>.</summary>
    public static string[] TlsModeLabels() => new[]
    {
        TlsModeLabel(ImportTlsMode.Implicit),
        TlsModeLabel(ImportTlsMode.StartTls),
    };
}
