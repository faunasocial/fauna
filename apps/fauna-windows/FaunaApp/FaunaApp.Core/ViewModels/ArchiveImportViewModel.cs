using System;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Services;
using uniffi.fauna_archive_import_machine;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>Which wizard screen is showing — the public mirror of the UniFFI-internal
/// <c>ArchiveImportStep</c>, so the panel can gate group visibility without referencing
/// the internal enum (the <see cref="MailImportStep"/> precedent).</summary>
public enum ArchiveImportScreen { Source, Archive, Scope, Confirm, Progress, Done }

/// <summary>One row of <c>archive-import-scope-categories</c>
/// (indexed <c>archive-import-scope-category-item</c>), projected from the shared
/// machine's <c>CategoryRow</c>. An importable row is a checkbox; a model-only one is
/// kept in the archive folder and never selectable (archive-import.md § What each
/// category becomes), so it paints as text with <c>state</c> <c>kept</c>.</summary>
public sealed class ArchiveImportCategoryItem
{
    public required string Token { get; init; }
    public required string Text { get; init; }
    public required bool Importable { get; init; }
    public required bool Selected { get; init; }
    /// <summary>The row's <c>state</c> attribute (<c>on</c>/<c>off</c>/<c>kept</c>), read by
    /// the e2e walk through <c>AutomationProperties.HelpText</c>
    /// (<c>actions/archive_import.py::category_state</c>).</summary>
    public string State => !Importable ? "kept" : Selected ? "on" : "off";
}

/// <summary>One row of <c>archive-import-category-progress-list</c> on the Progress
/// screen: a selected importable category and its settled counts.</summary>
public sealed class ArchiveImportProgressItem
{
    public required string Name { get; init; }
    public required string Progress { get; init; }
}

/// <summary>
/// The user-facing <c>archive-import</c> wizard (docs/goal/behavior/archive-import.md
/// § The wizard and its machine): Source → Archive → Scope → Confirm → Progress → Done,
/// importing a Facebook or Instagram export archive at its original dates. A dumb
/// projection over the shared <c>fauna_archive_import_machine::ArchiveImportMachine</c>,
/// consumed through its UniFFI-generated <see cref="IArchiveImportMachine"/> interface
/// (machine-as-seam): the FSM, the parser, the folder writes and the authoring are shared
/// Rust (priority #2), and every word this page shows for a token — a category, a run
/// state, an audience mode, a platform's help — comes from the machine crate's shared
/// vocabulary fns. Mirrors <see cref="MailImportViewModel"/>; lifts
/// apps/fauna-tui/src/settings/archive_import.rs.
///
/// <para><b>This VM spawns the drive loop itself.</b> The machine never self-spawns
/// <c>run_import</c>: after a <c>Start</c>/<c>Resume</c> that leaves the run
/// <c>Running</c>, <see cref="ShouldDriveImport"/> goes true and the panel fires
/// <see cref="DriveImportAsync"/> plus a repaint tick — the same as a hydrate that finds
/// a run still <c>Running</c>.</para>
///
/// <para><b>The path and date fields are page-local drafts</b>, committed in one ordered
/// dispatch at Open archive / Next (the tui <c>open_actions</c> /
/// <c>scope_next_actions</c> shape), never a <c>Set*</c> per keystroke.</para>
/// </summary>
public partial class ArchiveImportViewModel : ObservableObject
{
    private readonly IArchiveImportMachine _machine;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    [ObservableProperty] private ArchiveImportScreen _screen;
    /// <summary>Source-picker selection: 0 = Facebook, 1 = Instagram.</summary>
    [ObservableProperty] private int _sourceKindIndex;

    /// <summary>The imported-posts selection's source token for the chosen archive's
    /// platform — the lowercase <c>facebook</c> / <c>instagram</c> token
    /// <c>FeedManager.select_imported</c> takes (feed.md § The imported-posts selection).</summary>
    public string ImportedSourceToken => SourceKindIndex == 1 ? "instagram" : "facebook";
    [ObservableProperty] private string _sourceHelp = string.Empty;
    /// <summary>Step 1's deep link: the chosen platform's export page — the snapshot's
    /// derived <c>source_export_url</c>.</summary>
    [ObservableProperty] private string _sourceExportUrl = string.Empty;
    [ObservableProperty] private string _sourceExportLinkText = string.Empty;
    /// <summary>The snapshot's own <c>archive_path</c> — read only to re-seed the path
    /// draft when a hydrate lands on a resumable run, never by the general render.</summary>
    public string ArchivePath { get; private set; } = string.Empty;

    [ObservableProperty] private string _archiveSummary = string.Empty;
    /// <summary>Audience-mode picker selection: 0 = original, 1 = only me.</summary>
    [ObservableProperty] private int _audienceModeIndex;
    [ObservableProperty] private string _audienceSummary = string.Empty;
    /// <summary>The nest predates hidden tiers: only public posts import now.</summary>
    [ObservableProperty] private bool _hiddenTiersUnavailable;

    [ObservableProperty] private string _confirmSummary = string.Empty;
    [ObservableProperty] private string _progressSummary = string.Empty;
    [ObservableProperty] private double _progressFraction;
    [ObservableProperty] private string _errorLog = string.Empty;
    [ObservableProperty] private string _doneSummary = string.Empty;
    [ObservableProperty] private string _folderLinkText = string.Empty;
    /// <summary>Done step's profile prefill is offered: the archive carries a profile and it
    /// has not been applied yet (the snapshot's <c>archive_profile</c> / <c>profile_applied</c>).</summary>
    [ObservableProperty] private bool _profilePrefillAvailable;
    [ObservableProperty] private bool _canPause;
    [ObservableProperty] private bool _canResume;
    /// <summary>A cancelled or completed run may be left (Back starts a fresh wizard); a
    /// running, paused or errored one is resumable and has no Back.</summary>
    [ObservableProperty] private bool _progressConcluded;

    /// <summary>True once a dispatch (or a hydrate) left the run <c>Running</c> with no
    /// drive loop yet; the panel reads it to spawn <see cref="DriveImportAsync"/> exactly
    /// once. Cleared by the loop's own completion.</summary>
    public bool ShouldDriveImport { get; private set; }

    public ObservableCollection<ArchiveImportCategoryItem> Categories { get; } = new();
    public ObservableCollection<ArchiveImportProgressItem> CategoryProgress { get; } = new();

    /// <summary>The account's archive links, attached to the machine before every run
    /// (<see cref="DriveImportAsync"/>); <c>null</c> while none are wired.</summary>
    private readonly Func<ArchiveLinks?> _links;

    internal ArchiveImportViewModel(IArchiveImportMachine machine, Func<ArchiveLinks?>? links = null)
    {
        _machine = machine;
        _links = links ?? (() => ArchiveLinksHost.Instance.Current as ArchiveLinks);
    }

    /// <summary>Initial page load: hydrate — which finds an unfinished import in an
    /// archive folder and lands on its Progress step — then project.</summary>
    public async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            await _machine.Hydrate();
        }
        catch (Exception ex)
        {
            if (string.IsNullOrEmpty(Error)) Error = Strings.Error(ex);
        }
        finally
        {
            IsLoading = false;
        }
        var snap = _machine.Snapshot();
        Apply(snap);
        if (snap.runState == RunState.Running) ShouldDriveImport = true;
    }

    // ── Step 1 — Source ──
    public Task SelectSourceKindAsync(int index)
        => DispatchAsync(new ArchiveImportAction.SelectSource(KindAt(index)));

    // ── Step 2 — Archive ──

    /// <summary>Commit the path draft, then open it — one ordered dispatch.</summary>
    public async Task OpenArchiveAsync(string path)
    {
        if (await DispatchOneAsync(new ArchiveImportAction.SetArchivePath(path.Trim())))
        {
            await DispatchOneAsync(new ArchiveImportAction.OpenArchive());
        }
        Apply(_machine.Snapshot());
    }

    // ── Step 3 — Scope ──
    public Task ToggleCategoryAsync(string token, bool selected)
        => DispatchAsync(new ArchiveImportAction.ToggleCategory(token, selected));

    public Task SetAudienceModeAsync(int index)
        => DispatchAsync(new ArchiveImportAction.SetAudienceMode(index == 1 ? AudienceMode.OnlyMe : AudienceMode.Original));

    /// <summary>Scope → Confirm: both date drafts committed, then the advance — the
    /// machine refuses a malformed date or a reversed range here.</summary>
    public async Task ScopeNextAsync(string dateFrom, string dateTo)
    {
        var ok = await DispatchOneAsync(new ArchiveImportAction.SetDateFrom(dateFrom.Trim()))
            && await DispatchOneAsync(new ArchiveImportAction.SetDateTo(dateTo.Trim()));
        if (ok) await DispatchOneAsync(new ArchiveImportAction.Next());
        Apply(_machine.Snapshot());
    }

    // ── Navigation + lifecycle ──
    public Task NextAsync() => DispatchAsync(new ArchiveImportAction.Next());
    public Task BackAsync() => DispatchAsync(new ArchiveImportAction.Back());

    /// <summary>Step 4's durable commit: the folder is created and the run begins.</summary>
    public Task StartAsync() => DispatchAndMaybeDriveAsync(new ArchiveImportAction.Start());
    public Task ResumeAsync() => DispatchAndMaybeDriveAsync(new ArchiveImportAction.Resume());
    public Task PauseAsync() => DispatchAsync(new ArchiveImportAction.Pause());
    /// <summary>Cancel keeps what already landed (§ The wizard and its machine, step 5).</summary>
    public Task CancelAsync() => DispatchAsync(new ArchiveImportAction.Cancel());
    /// <summary>Done step: write the archived profile onto the user's own profile.</summary>
    public Task ApplyProfileAsync() => DispatchAsync(new ArchiveImportAction.ApplyArchiveProfile());

    /// <summary>The drive loop. Returns when the run finishes, pauses, cancels or fails.
    /// Awaited WITHOUT <c>ConfigureAwait(false)</c>: the continuation repaints the bound
    /// collections on the UI thread (a WinUI VM that configures the context away throws a
    /// silent COMException).</summary>
    public async Task DriveImportAsync()
    {
        try
        {
            // Phase two's hand-over (archive-import.md § Linking and merging, rulings 4
            // and 11): with the links attached, a run that completes puts the claimed /
            // unmatched counts on the snapshot before the Done step paints and runs the
            // announce round itself — so nothing is glued onto this spawn. Covers a
            // Start, a Resume and a hydrate that finds a run still Running alike; a
            // re-attach replaces the links (a re-login built new ones).
            if (_links() is { } links) _machine.AttachLinks(links);
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

    /// <summary>Re-project with no dispatch — the repaint tick while the drive loop runs.</summary>
    public void Repaint() => Apply(_machine.Snapshot());

    /// <summary>Arm the machine's one-shot pause (or hold) after <paramref name="records"/>
    /// settled records — the restart-resume and pause/cancel journeys' causal anchors
    /// (testing.md convention 14). The setters are a UniFFI seam the production FFI flavor
    /// does not export, so this forwarder compiles only where its one caller, the equally
    /// test-only <c>Testing.TestAgent</c>, does (the
    /// <c>OnboardingViewModel.CallMachineMethod</c> shape).</summary>
#if DEBUG || FAUNA_E2E_AGENT
    internal void ArmTestAnchor(bool hold, ulong records)
    {
        if (hold) _machine.SetHoldAfterRecordsForTest(records);
        else _machine.SetPauseAfterRecordsForTest(records);
    }
#endif

    private async Task DispatchAndMaybeDriveAsync(ArchiveImportAction action)
    {
        await DispatchOneAsync(action);
        var snap = _machine.Snapshot();
        Apply(snap);
        // Only an accepted transition arms the loop: a refused Start or Resume leaves the
        // run where it was, and spawning run_import on it would drive nothing.
        if (snap.runState == RunState.Running) ShouldDriveImport = true;
    }

    private async Task DispatchAsync(ArchiveImportAction action)
    {
        await DispatchOneAsync(action);
        Apply(_machine.Snapshot());
    }

    /// <summary>Dispatch one action; false when it was refused, so an ordered sequence
    /// stops at the first refusal. The machine has already recorded the refusal on its
    /// snapshot's <c>error</c>.</summary>
    private async Task<bool> DispatchOneAsync(ArchiveImportAction action)
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

    private void Apply(ArchiveImportSnapshot snap)
    {
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        Screen = ScreenOf(snap.step);
        SourceKindIndex = snap.sourceKind == ArchiveSourceKind.Instagram ? 1 : 0;
        SourceHelp = Strings.Resolve(FaunaArchiveImportMachineMethods.ArchiveImportSourceHelp(snap.sourceKind));
        SourceExportUrl = snap.sourceExportUrl;
        ArchivePath = snap.archivePath;

        // Every line is composed once in shared Rust (archive_import_page_text); this page
        // only resolves each ComposedText through the localizer.
        var text = FaunaArchiveImportMachineMethods.ArchiveImportPageText(snap);
        SourceExportLinkText = Strings.Resolve(text.sourceLink);
        ArchiveSummary = text.archiveSummary is { } archive ? Strings.Resolve(archive) : string.Empty;

        AudienceModeIndex = snap.audienceMode == AudienceMode.OnlyMe ? 1 : 0;
        AudienceSummary = Strings.Resolve(text.audienceSummary);
        HiddenTiersUnavailable = snap.nestSupportsHiddenTiers == false;

        Categories.Clear();
        CategoryProgress.Clear();
        for (var i = 0; i < snap.categories.Length; i++)
        {
            var row = snap.categories[i];
            Categories.Add(new ArchiveImportCategoryItem
            {
                Token = row.token,
                Text = Strings.Resolve(text.scopeRows[i]),
                Importable = row.importable,
                Selected = row.selected,
            });
        }
        foreach (var row in text.progressRows)
        {
            CategoryProgress.Add(new ArchiveImportProgressItem
            {
                Name = Strings.Resolve(row.label),
                Progress = Strings.Resolve(row.progress),
            });
        }

        ConfirmSummary = Strings.Resolve(text.confirmSummary);
        ProgressSummary = Strings.Resolve(text.progressSummary);
        ProgressFraction = text.progressFraction;
        ErrorLog = string.Join("\n", snap.skipLog);
        DoneSummary = Strings.Resolve(text.doneSummary);
        FolderLinkText = Strings.Resolve(text.folderLink);
        ProfilePrefillAvailable = snap.archiveProfile is not null && !snap.profileApplied;
        CanPause = snap.runState == RunState.Running;
        CanResume = snap.runState == RunState.Paused;
        ProgressConcluded = snap.runState is RunState.Cancelled or RunState.Completed;
    }

    private static ArchiveImportScreen ScreenOf(ArchiveImportStep s) => s switch
    {
        ArchiveImportStep.Archive => ArchiveImportScreen.Archive,
        ArchiveImportStep.Scope => ArchiveImportScreen.Scope,
        ArchiveImportStep.Confirm => ArchiveImportScreen.Confirm,
        ArchiveImportStep.Progress => ArchiveImportScreen.Progress,
        ArchiveImportStep.Done => ArchiveImportScreen.Done,
        _ => ArchiveImportScreen.Source,
    };

    private static ArchiveSourceKind KindAt(int index) =>
        index == 1 ? ArchiveSourceKind.Instagram : ArchiveSourceKind.Facebook;

    /// <summary>The two platform names in picker order, from the shared vocabulary
    /// (a brand name, never translated) — the picker's option text round-trips as its
    /// value, so <c>driver.select(…, "Facebook")</c> picks it.</summary>
    public static string[] SourceKindLabels() => new[]
    {
        FaunaArchiveImportMachineMethods.ArchiveImportSourceKindLabel(ArchiveSourceKind.Facebook),
        FaunaArchiveImportMachineMethods.ArchiveImportSourceKindLabel(ArchiveSourceKind.Instagram),
    };

    /// <summary>The two audience-mode labels in picker order.</summary>
    public static string[] AudienceModeLabels() => new[]
    {
        Strings.Resolve(FaunaArchiveImportMachineMethods.ArchiveImportAudienceModeLabel(AudienceMode.Original)),
        Strings.Resolve(FaunaArchiveImportMachineMethods.ArchiveImportAudienceModeLabel(AudienceMode.OnlyMe)),
    };
}
