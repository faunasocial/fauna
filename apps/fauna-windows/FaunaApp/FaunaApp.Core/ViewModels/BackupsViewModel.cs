using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using uniffi.fauna_backups_machine;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Backups page's view model. Three halves, deliberately different in kind:
///
/// <para><b>Snapshot half</b> — a dumb projection over the shared-Rust
/// <c>fauna-backups-machine</c>, consumed through its UniFFI-generated
/// <see cref="IBackupsMachine"/> interface (machine-as-seam, the
/// <c>MailListsViewModel</c> idiom). ALL page logic — the selector source
/// (<c>fauna.folders.list</c>, owner-scoped), page-lifetime selection with a
/// deterministic default, the <c>last_backed_up</c> derivation, single-flight,
/// the check verdict, the prune preview, per-row integrity, the detail read —
/// lives in the machine (<c>ui/backups.md</c> § Snapshot-list shape;
/// Architectural rule 1: an app renders <c>backups_snapshot()</c> and dispatches
/// gestures, it holds no page logic). This VM re-projects the snapshot into
/// bindable rows and forwards gestures; it decides nothing.</para>
///
/// <para><b>Destination half</b> and <b>restore half</b> — unchanged, over the
/// shared FFI mutate seam + the <c>FfiSnapshotsClient</c> restore reads
/// (backups.md §§ Manage backup destinations / Restore …).</para>
///
/// <para>The pre-adoption snapshot half is what this replaced: per-call
/// <c>fauna.filesync.snapshot.*</c> RPCs over a <c>fauna.sync.backup_status</c>
/// selector, a <c>"Last: "</c> prefix, a client-supplied daily/weekly/monthly
/// prune triple, a hand-rolled immediate-delete predicate, raw record
/// <c>ToString()</c> rows and no delete confirm — the last hand-rolled copy of
/// each, per the § Snapshot-list shape reconciliation ledger.</para>
/// </summary>
public partial class BackupsViewModel : ViewModelBase
{
    /// The shared page machine (snapshot half). Null until the page attaches one
    /// (<see cref="AttachMachine"/>) — it needs a CONNECTED WS-RPC client, which
    /// the synchronous constructor cannot await — and in the destination/restore
    /// unit tests, which never touch the snapshot half. Every snapshot-half
    /// member below reads as empty while it is null.
    private IBackupsMachine? _machine;

    [ObservableProperty] private bool _isLoading;

    /// <summary>Single-flight (§ *Create* ruling): true while ANY machine op is in
    /// flight, disabling every mutating control. One predicate, so no control can
    /// drift off it — this replaces the create-only <c>IsCreatingSnapshot</c> flag.</summary>
    [ObservableProperty] private bool _isBusy;

    /// <summary>The in-flight op's line ("Creating a snapshot…"), or null when
    /// idle: a disabled control the user can see states why.</summary>
    [ObservableProperty] private string? _busyText;

    /// <summary><c>last-backed-up</c> — ONE non-indexed element off the machine's
    /// derivation (the selected set's newest snapshot <c>created_at</c>, "never"
    /// when it has none). The old windows <c>"Last: "</c> prefix and its
    /// first-row-of-the-list derivation are both gone.</summary>
    [ObservableProperty] private string _lastBackedUpText = Strings.Get("backups/last_backed_up_never");

    /// <summary>The check verdict, off the shared <c>is_ok</c> predicate — a RESULT
    /// surface, never <c>error-message</c> (§ Architectural rules, rule 6). Null
    /// until a check runs this session.</summary>
    [ObservableProperty] private string? _checkResultText;

    /// <summary>Whether a prune dry-run is standing. Execute is offered ONLY from
    /// a preview (§ *Prune* ruling — structural, not a UI convention).</summary>
    [ObservableProperty] private bool _hasPrunePreview;

    /// <summary>Whether the preview names candidates the execute would actually
    /// remove: an armed execute over zero candidates would promise an effect it
    /// cannot have. False for both no-op policy states.</summary>
    [ObservableProperty] private bool _pruneExecutable;

    /// <summary>The snapshot id whose <c>snapshot-detail-files</c> list is open,
    /// or null. Read by the per-file download (the bytes are fetched by snapshot
    /// id + path).</summary>
    [ObservableProperty] private long? _openSnapshotId;

    /// <summary>The folder selector's options — the machine's owner-scoped,
    /// reserved-excluded, name-ordered <c>fauna.folders.list</c> read. The
    /// retired <c>fauna.sync.backup_status</c> source rendered a set's last FILE
    /// CHANGE, not its last snapshot.</summary>
    public ObservableCollection<string> FolderNames { get; } = new();

    /// <summary>The machine's current selection (page-lifetime, deterministic
    /// default). The page re-points its picker at this under a re-entry guard.</summary>
    [ObservableProperty] private string? _selectedFolder;

    /// <summary>The selected set's rows, wire order (newest-first — apps do not
    /// re-sort).</summary>
    public ObservableCollection<SnapshotDisplayRow> Snapshots { get; } = new();

    /// <summary>The open snapshot's file rows (empty when none is open).</summary>
    public ObservableCollection<SnapshotFileDisplayRow> DetailFiles { get; } = new();

    /// <summary>The prune preview's rendered lines: the policy-state sentence or
    /// the would-prune/remaining counts, then one line per candidate.</summary>
    public ObservableCollection<string> PrunePreviewLines { get; } = new();

    /// <summary>The configured backup destinations (backups.md § Manage backup
    /// destinations). Repopulated from the shared FFI mutate seam on every
    /// list/add/edit/remove — the page renders one indexed
    /// <c>backup-destination-status-row</c> per entry.</summary>
    public ObservableCollection<BackupDestinationRow> Destinations { get; } = new();

    /// Stable locale-agnostic token <c>backup_destination_edit</c> raises when a
    /// URL change points at a different nest identity (see backup_destinations.rs
    /// <c>EDIT_DIFFERENT_NEST_ERR</c>) — mapped to the localized hint.
    private const string EditDifferentNestToken = "backup-destination-edit-different-nest";

    private readonly INestRpcClient _nest;

    /// The device's stable sync device id (hex) — used for device-scoped calls this
    /// page still makes directly (single-file restore, client-device custodian
    /// enrollment). The per-destination status read itself no longer needs it (the
    /// nest derives the owner from the authenticated connection — the retired
    /// `(device_id, data_dir)` canonical-path contract was the always-on upload
    /// driver's, deleted with it; backups.md § Per-destination status read). Plumbed
    /// by the page from <c>ISessionAccount.DeviceId</c>; empty in unit tests.
    private readonly string _deviceId;

    /// LIVE per-destination status keyed by destination_id (backups.md § Per-destination
    /// status read). A destination with no entry (read not yet done, or it degraded on a
    /// transient socket hiccup) renders the not-yet-backed-up baseline ("never" /
    /// "0 queued"), uniform with apple/linux.
    private IReadOnlyDictionary<string, FfiBackupDestinationStatus> _statuses =
        new Dictionary<string, FfiBackupDestinationStatus>();

    /// The RAW <c>FfiBackupDestinationView</c> list from the last repopulate — kept
    /// alongside the display-projected <see cref="Destinations"/> because
    /// <see cref="SoleClientWarningVisible"/> feeds the shared
    /// <c>every_destination_is_a_client_device</c> predicate, which must read the
    /// row's own kind/device-id/cap, never a reconstruction from the display row
    /// (backups.md § Third destination kind).
    private IReadOnlyList<FfiBackupDestinationView> _destinationViews =
        new List<FfiBackupDestinationView>();

    /// LIVE per-destination audit outcome keyed by destination_id (backups.md §
    /// Audit-alert surface). A destination with no entry (read not yet done, or a
    /// transient degrade) renders the "never checked" baseline and no alert.
    private IReadOnlyDictionary<string, FfiDestinationAuditRow> _auditRows =
        new Dictionary<string, FfiDestinationAuditRow>();

    /// The actor-scoped audit-state file this shell's client-side audit pass
    /// reads/writes (<see cref="AccountStateDir.BackupAuditStatePath"/>). Empty in
    /// unit tests / before the actor is known — the audit read/observe calls are
    /// then skipped, same convention as <see cref="_deviceId"/>.
    private readonly string _auditStatePath;

    /// The sync agent's per-actor replica dir the audit pass anchors the
    /// covered-folder mirror plane in (<see cref="AccountStateDir.SyncAgentStateDir"/>;
    /// backup-destinations.md § Ordinary-folder coverage → <i>Retention + audit</i>).
    /// Null in unit tests / before the actor is known — the declared absence, under
    /// which that plane keeps presence over the destination's list.
    private readonly string? _syncStateDir;

    /// The <c>backup-audit-alert</c> indexed banners — one per destination
    /// currently failing its audit, rendered above the destinations/snapshot
    /// scroller (the <c>restore-divergence-banner</c> idiom; backups.md § Audit-
    /// alert surface). Empty while every destination's last audit passed.
    public ObservableCollection<string> AuditAlerts { get; } = new();

    /// The single-file-restore save step (backups.md § Where logic lives →
    /// *Single-file byte download*) — the only part of that leg that is platform
    /// glue; the bytes themselves come off the shared walk via `_nest`. Null in
    /// unit tests / callers without the download surface — the download command is
    /// then a no-op.
    private readonly ISnapshotFileSaver? _fileSaver;

    /// The live sync-agent control channel, resolved per call — the
    /// <c>AgentStatusProbe</c> / <c>AgentSyncNudge</c> idiom
    /// (<c>() =&gt; App.CurrentSyncAgent?.Channel</c>), never a captured channel:
    /// the agent session is rebuilt on every login and nest re-point, so holding
    /// one would pin a dead handle. Null (or a null return) means this device
    /// drives no agent — see <see cref="RefreshOrphanedStoreAsync"/> for why that
    /// answers "not orphaned" rather than "unknown".
    private readonly Func<IFfiSyncAgentProvisioner?>? _agentChannel;

    internal BackupsViewModel(
        INestRpcClient nest, string deviceId = "",
        ISnapshotFileSaver? fileSaver = null, string auditStatePath = "",
        Func<IFfiSyncAgentProvisioner?>? agentChannel = null,
        string? syncStateDir = null)
    {
        _nest = nest;
        _deviceId = deviceId;
        _fileSaver = fileSaver;
        _auditStatePath = auditStatePath;
        _syncStateDir = syncStateDir;
        _agentChannel = agentChannel;
    }

    /// <summary>Attach the shared page machine (built by the page against a
    /// connected WS-RPC client) and project its current state. Separate from the
    /// constructor because the build is async; unit tests pass a fake here.</summary>
    internal void AttachMachine(IBackupsMachine machine)
    {
        _machine = machine;
        Apply();
    }

    /// <summary>Re-project the machine's state — the observer tick's landing
    /// point. Every gesture below also re-projects on completion, so a VM driven
    /// without an observer (unit tests) stays correct.</summary>
    internal void Apply()
    {
        if (_machine is null) return;
        var snap = _machine.Snapshot();

        IsBusy = snap.inProgressOp is not null;
        BusyText = snap.inProgressOp is { } op ? BusyTextFor(op) : null;

        // The selector's options + the machine's selection. Assigning
        // SelectedFolder raises the page's re-entry-guarded re-point; the page
        // never dispatches a select for a selection the machine already holds.
        FolderNames.Clear();
        foreach (var fs in snap.folders) FolderNames.Add(fs.name);
        SelectedFolder = snap.selectedFolder;

        LastBackedUpText = snap.lastBackedUp is { } at
            ? Strings.Get("backups/last_backed_up_at").Replace("{when}", FormatEpochSeconds(at))
            : Strings.Get("backups/last_backed_up_never");

        Snapshots.Clear();
        foreach (var row in snap.snapshots)
            Snapshots.Add(new SnapshotDisplayRow(
                row.id, SnapshotRowText(row), !IsBusy, row.state is SnapshotState.SoftDeleted));

        CheckResultText = snap.checkResult is { } result ? CheckResultTextFor(result) : null;
        ApplyPrunePreview(snap.prunePreview);

        DetailFiles.Clear();
        OpenSnapshotId = snap.detail?.snapshotId;
        if (snap.detail is { } detail)
        {
            foreach (var f in detail.files)
            {
                DetailFiles.Add(new SnapshotFileDisplayRow(
                    f.path,
                    ValueFormat.ByteSize((ulong)Math.Max(0, f.sizeBytes)),
                    // The download affordance is a regular-file gesture; a
                    // directory / symlink row renders as a row only (tui's shape).
                    f.fileType == "regular"));
            }
        }

        // The machine localizes the last read/gesture failure into snapshot.error
        // and clears it on the next success. It NEVER carries a success — a
        // completed prune or check reports through its own result surface.
        SetError(snap.error is { } err ? Strings.Resolve(err) : null);
    }

    /// The visible <c>snapshot-item</c> line (§ *Row content contract*): formatted
    /// created-at, file count and formatted size, plus a non-<c>Active</c>
    /// lifecycle state and the session-derived integrity verdict. Static + pure so
    /// unit tests pin the contract without a live machine. The lifecycle +
    /// integrity suffixes route through the shared
    /// <c>fauna_backups_machine::{snapshot_state_text, snapshot_integrity_text}</c>
    /// (via <c>FaunaFfiMethods</c>) — this view model owns only the timestamp
    /// formatting and which of a state's own fields carries the deadline; the
    /// shared fns own which i18n key and the dated/undated fallback
    /// (<c>docs/goal/ui/backups.md</c> § Where logic lives).
    internal static string SnapshotRowText(SnapshotRow row)
    {
        var text = Strings.Get("backups/snapshot_row")
            .Replace("{id}", row.id.ToString())
            .Replace("{when}", FormatEpochSeconds(row.createdAt))
            .Replace("{files}", Strings.Get("backups/file_count")
                .Replace("{count}", Math.Max(0, row.fileCount).ToString()))
            .Replace("{size}", ValueFormat.ByteSize((ulong)Math.Max(0, row.totalBytes)));

        // A non-Active state renders ON the row, with the deadline the user can
        // still act on — that deadline is why the lifecycle fields exist. An
        // undated state still renders; inventing a date (or hiding the state) is
        // the failure to avoid.
        long? deadline = row.state switch
        {
            SnapshotState.DeletionPending { executeAfter: { } at } => at,
            SnapshotState.SoftDeleted { purgeAfter: { } at } => at,
            _ => null,
        };
        var formattedDeadline = deadline is { } d ? FormatEpochSeconds(d) : null;
        if (FaunaFfiMethods.SnapshotStateText(row.state, formattedDeadline) is { } stateText)
            text += "  " + Strings.Resolve(stateText);

        // Integrity is ABSENT until a check runs this session — Unknown paints
        // nothing rather than the word "unknown", which would read as a finding.
        if (FaunaFfiMethods.SnapshotIntegrityText(row.integrity) is { } integrityText)
            text += "  " + Strings.Resolve(integrityText);

        return text;
    }

    /// The check verdict line — the shared <c>is_ok</c> field, READ, never
    /// re-derived from <c>status == "ok"</c> or from error counts (ratified
    /// 2026-06-01). A completed check with findings is a result, not an error.
    internal static string CheckResultTextFor(CheckOutcome result) =>
        result.isOk
            ? Strings.Get("backups/check_result_ok")
                .Replace("{snapshots}", result.snapshotsChecked.ToString())
                .Replace("{files}", result.filesChecked.ToString())
                .Replace("{chunks}", result.chunksChecked.ToString())
            : Strings.Get("backups/check_result_errors")
                .Replace("{missing_manifests}", result.missingManifests.ToString())
                .Replace("{missing_chunks}", result.missingChunks.ToString())
                .Replace("{corrupt_manifests}", result.corruptManifests.ToString());

    /// The single-flight indicator's line for the running op, off the shared
    /// <c>fauna_backups_machine::busy_text</c> (via
    /// <c>FaunaFfiMethods.BusyText</c>) — no app hand-rolls which key an
    /// operation maps to (<c>docs/goal/ui/backups.md</c> § Where logic lives).
    private static string BusyTextFor(BackupOp op) => Strings.Resolve(FaunaFfiMethods.BusyText(op));

    /// Project the prune dry-run. The two no-op policy states say WHY nothing
    /// would be pruned rather than showing an empty success, and neither arms the
    /// execute.
    private void ApplyPrunePreview(PrunePreview? preview)
    {
        PrunePreviewLines.Clear();
        if (preview is null)
        {
            HasPrunePreview = false;
            PruneExecutable = false;
            return;
        }
        HasPrunePreview = true;
        var executable = false;
        switch (preview.policyState)
        {
            case PolicyState.NotSet:
                PrunePreviewLines.Add(Strings.Get("backups/prune_policy_not_set"));
                break;
            case PolicyState.Unparseable:
                PrunePreviewLines.Add(Strings.Get("backups/prune_policy_unparseable"));
                break;
            default:
                if (preview.candidates.Length == 0)
                {
                    PrunePreviewLines.Add(Strings.Get("backups/prune_preview_nothing"));
                }
                else
                {
                    PrunePreviewLines.Add(Strings.Get("backups/prune_preview_counts")
                        .Replace("{would_prune}", preview.wouldPrune.ToString())
                        .Replace("{remaining}", preview.remaining.ToString()));
                    foreach (var c in preview.candidates)
                    {
                        PrunePreviewLines.Add(Strings.Get("backups/prune_preview_candidate")
                            .Replace("{id}", c.id.ToString())
                            .Replace("{when}", FormatEpochSeconds(c.createdAt)));
                    }
                    executable = !IsBusy;
                }
                break;
        }
        PruneExecutable = executable;
    }

    /// <summary>The page's mount read — the machine loads the selector and the
    /// selected set's snapshots.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        try
        {
            await DispatchAsync(m => m.Refresh());
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Open one snapshot's file list (<c>snapshot-item[i]</c> click →
    /// <c>snapshot-detail-files</c>). The read is the machine's custody-wired
    /// <c>SnapshotsClient::get</c>, so a sealed set's paths arrive opened without
    /// this page wiring custody a second time.</summary>
    public Task OpenSnapshotAsync(long snapshotId) => DispatchAsync(m => m.OpenSnapshot(snapshotId));

    /// <summary>Close the open file list (local state only, no round trip).</summary>
    public void CloseSnapshotDetail()
    {
        _machine?.CloseSnapshotDetail();
        Apply();
    }

    /// <summary>Single-file restore (`snapshot-file-download-button[i]`,
    /// backup-restore.md § 3): fetch one file's decrypted bytes from the selected
    /// snapshot via the shared-Rust client-side walk, then save through the
    /// <see cref="ISnapshotFileSaver"/> seam (native dialog in production; a
    /// fixed directory under e2e). A cancelled dialog is not an error.
    /// <para>The bytes come off <c>download_snapshot_file_bytes</c> — NOT the legacy
    /// per-app HTTP route, which backups.md § Where logic lives says must gain no
    /// callers (it is bearer-authed and refuses sealed manifests).</para></summary>
    [RelayCommand]
    private async Task DownloadSnapshotFileAsync(SnapshotFileDisplayRow file)
    {
        if (OpenSnapshotId is not { } snapshotId || _fileSaver is null) return;
        ErrorMessage = null;
        try
        {
            var data = await _nest.DownloadSnapshotFileBytesAsync(
                _deviceId, (ulong)snapshotId, file.Path);
            // The dialog's name field takes a basename, never a snapshot path.
            var slash = file.Path.LastIndexOf('/');
            var name = slash >= 0 ? file.Path[(slash + 1)..] : file.Path;
            await _fileSaver.SaveAsync(name, data);
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary><c>snapshot-create-button</c> — take a manual snapshot of the
    /// selected set. The machine's create seam has NO <c>tags</c> parameter at all
    /// (a structural ruling, § *Create*), which is what retired windows'
    /// <c>["manual"]</c> tag for good: a tag is a retention SHIELD, so tagging
    /// manual snapshots exempted them from the user's own retention forever.
    /// Nothing here can regress it — there is no argument to pass.</summary>
    [RelayCommand]
    private Task CreateSnapshotAsync() => DispatchAsync(m => m.CreateSnapshot());

    /// <summary>The <c>backup-folder-selector</c> gesture. The machine clears the
    /// session-local check result and any standing prune preview — both are scoped
    /// to the set they were produced for.</summary>
    [RelayCommand]
    private Task SelectFolderAsync(string folder) => DispatchAsync(m => m.SelectFolder(folder));

    /// <summary><c>snapshot-delete-button[i]</c> — queue the 48 h soft delete; the
    /// row comes back as <c>DeletionPending</c>. The confirm is client glue and
    /// lives on the page.</summary>
    [RelayCommand]
    private Task DeleteSnapshotAsync(long id) => DispatchAsync(m => m.DeleteSnapshot(id));

    /// <summary><c>snapshot-undelete-button[i]</c> — recover a soft-deleted
    /// snapshot before its <c>purge_after</c> (§ *Soft-deleted rows*, id
    /// user-approved 2026-08-14). No confirm — the page only renders the control
    /// on a <c>SoftDeleted</c> row, and the machine refuses the gesture on any
    /// other row regardless, so the render guard IS the affordance rule (mirrors
    /// android/web/linux, none of which confirm this gesture either).</summary>
    [RelayCommand]
    private Task UndeleteSnapshotAsync(long id) => DispatchAsync(m => m.UndeleteSnapshot(id));

    /// <summary><c>snapshot-prune-button</c> — the PREVIEW half: a dry run of the
    /// set's own resting retention policy. It never carries a client-chosen policy
    /// (Architectural rule 5), which is what retired windows' daily/weekly/monthly
    /// triple — the page was overriding whatever the owner had configured for the
    /// set.</summary>
    [RelayCommand]
    private Task PruneAsync() => DispatchAsync(m => m.PrunePreview());

    /// <summary>Execute the previewed prune. A call with no preview standing is a
    /// machine-side no-op, which is what makes preview-first structural.</summary>
    public Task PruneExecuteAsync() => DispatchAsync(m => m.PruneExecute());

    /// <summary>Dismiss a standing prune preview without executing it.</summary>
    public void CancelPrunePreview()
    {
        _machine?.CancelPrunePreview();
        Apply();
    }

    /// <summary><c>snapshot-check-button</c> — run the integrity check directly
    /// (one click runs it). A check that COMPLETES WITH FINDINGS is a result, not
    /// an error: it lands in <see cref="CheckResultText"/> and implicates its rows,
    /// and <c>error-message</c> stays clear (Architectural rule 6). The old windows
    /// path pushed the findings into the error banner.</summary>
    [RelayCommand]
    private Task CheckIntegrityAsync() => DispatchAsync(m => m.Check());

    /// Run one machine gesture and re-project. The machine localizes its own
    /// failures into <c>snapshot.error</c>, so the catch here is only for a
    /// boundary throw (a dropped connection mid-call); it still re-projects, so a
    /// partially-applied state never sticks.
    private async Task DispatchAsync(Func<IBackupsMachine, Task> gesture)
    {
        if (_machine is null) return;
        try
        {
            await gesture(_machine);
            Apply();
        }
        catch (Exception ex)
        {
            Apply();
            if (string.IsNullOrEmpty(ErrorMessage)) ShowError(ex);
        }
    }

    /// Epoch seconds → the shared fixed local "YYYY-MM-DD HH:MM" render
    /// (value-formatting.md § Absolute local timestamp display), shared with
    /// the restore-history rows below.
    private static string FormatEpochSeconds(long secs) =>
        FaunaFfiMethods.FormatUnixLocal(secs);

    // ── Backup destinations (management) ────────────────────────────────
    // Thin glue over the shared FFI mutate seam (backups.md § Where logic
    // lives): each call repopulates Destinations from the freshly-persisted
    // list the seam returns. The add/edit/remove methods return whether they
    // succeeded so the page can close its dialog only on success.

    /// <summary>Hydrate the destination rows from <c>backup_destinations_list</c>.</summary>
    public async Task LoadDestinationsAsync()
    {
        ErrorMessage = null;
        try
        {
            RepopulateDestinations(await _nest.BackupDestinationsListAsync());
            await RefreshStatusesAsync();
            await RefreshAuditAsync();
            // The orphaned verdict rides the SAME triggers as the two reads
            // above — mount, re-map and every mutation — never a render path.
            await RefreshOrphanedStoreAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Enroll a destination (<c>backup_destination_add</c>): resolve
    /// identity + verify reachability, then persist. Blank <paramref name="name"/>
    /// defaults to the destination domain.</summary>
    public async Task<bool> AddDestinationAsync(string url, string name)
    {
        ErrorMessage = null;
        try
        {
            RepopulateDestinations(await _nest.BackupDestinationAddAsync(url.Trim(), name.Trim()));
            await RefreshStatusesAsync();
            await RefreshAuditAsync();
            // The orphaned verdict rides the SAME triggers as the two reads
            // above — mount, re-map and every mutation — never a render path.
            await RefreshOrphanedStoreAsync();
            return true;
        }
        catch (Exception ex)
        {
            MapDestinationError(ex);
            return false;
        }
    }

    /// <summary>Rename / change a destination's URL (<c>backup_destination_edit</c>).
    /// A URL pointing at a different nest surfaces the localized
    /// remove-and-re-add hint.</summary>
    public async Task<bool> EditDestinationAsync(string id, string url, string name)
    {
        ErrorMessage = null;
        try
        {
            RepopulateDestinations(await _nest.BackupDestinationEditAsync(id, url.Trim(), name.Trim()));
            await RefreshStatusesAsync();
            await RefreshAuditAsync();
            // The orphaned verdict rides the SAME triggers as the two reads
            // above — mount, re-map and every mutation — never a render path.
            await RefreshOrphanedStoreAsync();
            return true;
        }
        catch (Exception ex)
        {
            MapDestinationError(ex);
            return false;
        }
    }

    /// <summary>Drop a destination (<c>backup_destination_remove</c>) — a plain
    /// config edit; the coordinator reconciles the offsite deregistration.
    ///
    /// <para>When <paramref name="alsoReclaim"/> is set
    /// (<c>backup-destination-remove-reclaim-checkbox</c>, offered only on
    /// client-device rows), this device's whole sealed custodian store is freed in
    /// the same gesture — <b>strictly AFTER the removal succeeds, never
    /// before</b>: the removal is what makes the store orphaned, and freeing it
    /// first would leave a live custody row pointing at bytes already gone if the
    /// deregister then failed. Mirrors linux
    /// <c>remove_destination_and_maybe_reclaim</c> and tui's <c>Op::Remove</c>
    /// ordering.</para></summary>
    public async Task<bool> RemoveDestinationAsync(string id, bool alsoReclaim = false)
    {
        ErrorMessage = null;
        try
        {
            RepopulateDestinations(await _nest.BackupDestinationRemoveAsync(id));
            await RefreshStatusesAsync();
            await RefreshAuditAsync();
        }
        catch (Exception ex)
        {
            MapDestinationError(ex);
            return false;
        }

        // Past this point the REMOVAL LANDED. Every failure below therefore says
        // so before reporting itself: a bare reclaim error over a list that no
        // longer paints the removed row is the one reading a user cannot recover
        // from. The surviving store's own `backup-orphaned-store-row` is both the
        // honest state and the way to retry.
        if (alsoReclaim)
        {
            var reclaimFailure = await ReclaimStoreAsync();
            if (reclaimFailure is not null)
            {
                SetError(Strings.Format("backups/backup_reclaim_after_remove_failed", reclaimFailure));
                await RefreshOrphanedStoreAsync();
                return false;
            }
        }

        await RefreshOrphanedStoreAsync();
        return true;
    }

    /// <summary><b>Keep</b> — <c>backup-destination-keep-button</c> on a row an
    /// identity succession carried across (<c>succession-aftermath.md</c>
    /// § Adjudicating what the aftermath carries across). No confirm, unlike
    /// Remove: Keep is non-destructive and re-decidable (the row stays removable
    /// forever after). The list is swapped in whole from the shared door's
    /// at-rest re-read, so the mark stops rendering because the row now reads
    /// <c>unattested: false</c>, never because this VM flipped it optimistically
    /// (mirrors apple <c>BackupDestinationsVM.keep</c>).</summary>
    public async Task<bool> KeepDestinationAsync(string id)
    {
        ErrorMessage = null;
        try
        {
            RepopulateDestinations(await _nest.BackupDestinationKeepAsync(id));
            return true;
        }
        catch (Exception ex)
        {
            MapDestinationError(ex);
            return false;
        }
    }

    /// <summary>Enroll THIS device as a client custodian
    /// (<c>backup_destination_enroll_custodian</c>) — the second add path
    /// (backups.md § Third destination kind → Enrollment). No URL, no resolve
    /// step: a custodian has no address, so there is nothing to reach over the
    /// network. <paramref name="capacityCapBytes"/> is <c>null</c> for uncapped
    /// (a real choice, never a substituted default) — the caller parses the
    /// user-typed cap through the shared <c>parse_byte_size</c> before calling
    /// this (<see cref="TryParseCapacity"/>).</summary>
    public async Task<bool> EnrollCustodianAsync(string name, ulong? capacityCapBytes)
    {
        ErrorMessage = null;
        try
        {
            RepopulateDestinations(
                await _nest.BackupDestinationEnrollCustodianAsync(_deviceId, name.Trim(), capacityCapBytes));
            await RefreshStatusesAsync();
            await RefreshAuditAsync();
            // The orphaned verdict rides the SAME triggers as the two reads
            // above — mount, re-map and every mutation — never a render path.
            await RefreshOrphanedStoreAsync();
            return true;
        }
        catch (Exception ex)
        {
            MapDestinationError(ex);
            return false;
        }
    }

    private void RepopulateDestinations(IReadOnlyList<FfiBackupDestinationView> list)
    {
        _destinationViews = list;
        Destinations.Clear();
        foreach (var d in list)
        {
            Destinations.Add(new BackupDestinationRow(
                d.destinationId,
                d.destinationNestUrl,
                // Shared Rust: display name, else the URL host (one source of
                // truth across the six apps — backups.md § Where logic lives).
                FaunaFfiMethods.BackupDestinationLabel(d.displayName, d.destinationNestUrl),
                d.kind,
                d.custodianDeviceId,
                d.capacityCapBytes,
                d.unattested));
        }
    }

    // ── Third destination kind: client-device custodian (backups.md § Third
    // destination kind) — the kind catalog + per-row kind/usage text + the
    // page-level sole-client warning, all pure-consume of the shared FFI faces
    // (priority #2; no re-derived label or predicate).

    /// <summary><c>backup-destination-kind-select</c>'s catalog
    /// (<c>backup_destination_kind_options</c>) — the implemented kinds in paint
    /// order, each carrying the wire value the row records and the label to
    /// resolve. Pure/static: no state, so the page reads it directly when
    /// building the picker. <c>internal</c> because <c>BackupDestinationKindOption</c>
    /// is UniFFI-<c>internal</c> (the WinUI assembly sees it via
    /// <c>[InternalsVisibleTo]</c>, same as <c>Strings.Resolve</c>).</summary>
    internal static IReadOnlyList<BackupDestinationKindOption> KindOptions() =>
        FaunaFfiMethods.BackupDestinationKindOptions();

    /// <summary>The <c>client-device</c> wire discriminator
    /// (<c>destination_kind_client_device</c>) — so the shell never hard-codes
    /// the string a newer client's `default_lapse_tier()`-style private mirror
    /// would drift from.</summary>
    public static string ClientDeviceKind => FaunaFfiMethods.DestinationKindClientDevice();

    /// <summary>Parse a user-typed capacity cap (<c>parse_byte_size</c>) —
    /// liberal about input shape, strict about what counts as a number.
    /// <c>null</c> is a refusal the caller must surface, never silently
    /// defaulted (guessing a cap is how a device's disk fills).</summary>
    public static ulong? TryParseCapacity(string input) => FaunaFfiMethods.ParseByteSize(input);

    /// <summary>The <c>backup-destination-kind-badge</c> text for a row: the
    /// shared kind label, reading the row's OWN discriminator — an unrecognised
    /// kind (a newer client wrote it) renders as itself rather than collapsing
    /// into a generic word (backups.md § Third destination kind → Durability +
    /// labeling).</summary>
    public static string KindBadgeText(string kind) =>
        Strings.Resolve(FaunaFfiMethods.BackupDestinationKindLabel(kind));

    /// <summary>The <c>backup-destination-usage</c> text for a client-device
    /// row: held bytes against the user-set cap, via the shared
    /// <c>backup_usage_label</c>. Cap-reached is READ from the live status's
    /// <c>cap_state</c>, never inferred from <c>held &gt;= cap</c> — a pull pass
    /// that stopped at its cap ends BELOW it, so inference would render a
    /// stalled backup as healthy-with-room.</summary>
    public string UsageText(string destinationId, ulong? capacityCapBytes)
    {
        var status = _statuses.GetValueOrDefault(destinationId);
        var display = FaunaFfiMethods.BackupUsageLabel(status?.heldBytes, capacityCapBytes, status?.capState);
        var text = Strings.Resolve(display.label);
        if (display.held is { } held) text = text.Replace("{held}", Strings.Resolve(held));
        if (display.cap is { } cap) text = text.Replace("{cap}", Strings.Resolve(cap));
        return text;
    }

    /// <summary><c>backup-orphaned-store-row</c>'s sentence, or <c>null</c> while
    /// this device holds no orphaned store — the row paints iff this is non-null,
    /// and it is a CACHED verdict, never a live computation
    /// (see <see cref="RefreshOrphanedStoreAsync"/>).</summary>
    [ObservableProperty] private string? _orphanedStoreText;

    /// <summary>Whether the reclaim controls render at all
    /// (<c>backup-orphaned-store-row</c> + its
    /// <c>backup-destination-reclaim-button</c>).</summary>
    public bool OrphanedStoreVisible => OrphanedStoreText is not null;

    partial void OnOrphanedStoreTextChanged(string? value) =>
        OnPropertyChanged(nameof(OrphanedStoreVisible));

    /// <summary>Does <paramref name="destinationId"/> name a CLIENT-DEVICE row —
    /// i.e. should its remove dialog offer
    /// <c>backup-destination-remove-reclaim-checkbox</c>? Reads the row's own
    /// <c>FfiBackupDestinationView</c> through the shared
    /// <c>destination_row_is_a_client_device</c> predicate, never a raw kind
    /// string compare: a row of an unrecognised kind (a newer client wrote it)
    /// must answer NO, and re-deriving that here is exactly what would get it
    /// backwards.</summary>
    public bool RowIsAClientDevice(string destinationId)
    {
        var row = _destinationViews.FirstOrDefault(d => d.destinationId == destinationId);
        return row is not null && FaunaFfiMethods.DestinationRowIsAClientDevice(row);
    }

    /// <summary>Recompute whether this device holds an ORPHANED sealed custodian
    /// store — a full copy with no destination row left to justify it — and cache
    /// the verdict into <see cref="OrphanedStoreText"/>
    /// (<c>backups.md</c> § Manage backup destinations → *Reclaim this device's
    /// copy*).
    ///
    /// <para><b>Deliberately never computed on a render path.</b> It costs an
    /// agent IPC round trip on top of the disk walk the agent does, so it rides
    /// the same three triggers the status/audit reads do: mount, re-map, and after
    /// every mutation. A property getter that did this would run it once per
    /// binding pass.</para>
    ///
    /// <para><b>Every failure answers "not orphaned"</b> — no agent on this
    /// device, no device id yet, a refusing agent, an FFI fault. That is the
    /// conservative direction for a gesture that DELETES the owner's only offline
    /// copy: not knowing must never paint the reclaim button. (The same reason the
    /// verdict is the shared <c>custodian_store_is_orphaned</c> and not
    /// <c>custodian_assignment_for(..).is_none()</c>, which answers None for two
    /// rows naming this device and would offer to delete live custody.)</para></summary>
    public async Task RefreshOrphanedStoreAsync()
    {
        var channel = _agentChannel?.Invoke();
        if (channel is null || string.IsNullOrEmpty(_deviceId))
        {
            OrphanedStoreText = null;
            return;
        }

        try
        {
            var info = await channel.CustodianStore();
            var orphaned = FaunaFfiMethods.CustodianStoreIsOrphaned(
                _destinationViews.ToArray(), _deviceId, info.bytes > 0);
            if (!orphaned)
            {
                OrphanedStoreText = null;
                return;
            }
            // Two-level resolve, exactly as `UsageText` does it: the byte size
            // localizes on its own before it lands in the sentence's {held} slot.
            var display = FaunaFfiMethods.OrphanedStoreLabel(info.bytes);
            OrphanedStoreText = Strings.Resolve(display.label)
                .Replace("{held}", Strings.Resolve(display.held));
        }
        catch (Exception)
        {
            OrphanedStoreText = null;
        }
    }

    /// <summary>Free this device's whole sealed custodian store — the confirmed
    /// <c>backup-reclaim-confirm-button</c> action. Returns <c>null</c> on
    /// success, else the reason to surface.
    ///
    /// <para>A <c>still_hosting</c> refusal is a REPORTED OUTCOME, not an error:
    /// the agent's own custodian work would not stop in time, so nothing was
    /// deleted and the store is intact. It still comes back as a reason string —
    /// the user must learn nothing happened — but it is the one case where the
    /// surviving orphaned row is the expected next state rather than a
    /// failure.</para></summary>
    private async Task<string?> ReclaimStoreAsync()
    {
        var channel = _agentChannel?.Invoke();
        if (channel is null)
        {
            // Convention 11: a command this app cannot honour fails loudly, never
            // silently. Reachable only if the row painted between the agent
            // tearing down and this click — unlikely, but real.
            return Strings.Get("backups/backup_reclaim_no_agent");
        }
        try
        {
            var outcome = await channel.ReclaimCustodianStore();
            return outcome.stillHosting ? Strings.Get("backups/backup_reclaim_still_hosting") : null;
        }
        catch (Exception ex)
        {
            return ex is FfiException.General g ? g.msg : ex.Message;
        }
    }

    /// <summary>The standalone reclaim (<c>backup-reclaim-confirm-button</c> on
    /// <c>backup-reclaim-confirm-modal</c>), not tied to a removal. Surfaces its
    /// own failure on <c>error-message</c> and re-reads the verdict either way —
    /// a refused reclaim leaves the row standing, which is the honest state and
    /// the way to retry.</summary>
    public async Task<bool> ReclaimOrphanedStoreAsync()
    {
        ErrorMessage = null;
        var failure = await ReclaimStoreAsync();
        if (failure is not null) SetError(failure);
        await RefreshOrphanedStoreAsync();
        return failure is null;
    }

    // ── Restore after losing the nest (re-seed) ─────────────────────────
    // ui/backups.md § Restore after losing the nest; the ceremony is
    // backup-destinations.md § Re-seed. Where the gesture paints is the shared
    // backup_reseed_rows; the ceremony, its wait and its re-enrollment are the
    // agent's FFI face (macOS makes the same call). This only paints the result,
    // as linux's render_reseed and FaunaKit's applyReseed do.

    /// <summary><c>backup-destination-reseed-result</c>'s text — the running
    /// line while the ceremony runs, then the shared result lines. <c>null</c>
    /// (the view absent) before any run and after a stop, which renders on
    /// <c>error-message</c> instead.</summary>
    [ObservableProperty] private string? _reseedResultText;

    /// <summary>A restore is running: every <c>backup-destination-reseed-button</c>
    /// is disabled until it ends.</summary>
    [ObservableProperty] private bool _reseedRunning;

    /// <summary>Where <c>backup-destination-reseed-button</c> paints: <c>null</c>
    /// for <c>backup-orphaned-store-row</c>, a destination id for a
    /// <c>backup-destination-status-row</c>. The shared <c>backup_reseed_rows</c>
    /// decides (the <c>reseed_sources</c> filter linux and tui apply) over state
    /// this VM already caches — the raw list and the orphaned verdict — so a
    /// render path costs no round trip. No agent ⇒ no store ⇒ no row.</summary>
    private IReadOnlyList<string?> ReseedRows =>
        _agentChannel?.Invoke() is null
            ? Array.Empty<string?>()
            : FaunaFfiMethods.BackupReseedRows(
                _destinationViews.ToArray(), _deviceId, OrphanedStoreText is not null);

    /// <summary>Does the orphaned-store row carry the restore button?</summary>
    public bool ReseedOnOrphanedStore => ReseedRows.Contains(null);

    /// <summary>Does this destination row carry the restore button?</summary>
    public bool ReseedOnRow(string destinationId) => ReseedRows.Contains(destinationId);

    /// <summary>The confirmed restore (<c>backup-destination-reseed-confirm-button</c>).
    /// Returns whether it ended with nothing left to say — a verdict and a landed
    /// re-enrollment. A stop (the agent's own, or a fault reaching it) renders on
    /// <c>error-message</c> inside <c>backup_reseed_failed</c> and leaves the result
    /// view absent: nothing was made live, and re-running resumes.</summary>
    public async Task<bool> ReseedAsync()
    {
        if (ReseedRunning) return false;
        ErrorMessage = null;
        var channel = _agentChannel?.Invoke();
        if (channel is null)
        {
            // Convention 11: a command this app cannot honour fails loudly.
            SetError(Strings.Get("backups/backup_reseed_no_agent"));
            return false;
        }

        ReseedRunning = true;
        ReseedResultText = Strings.Get("backups/backup_reseed_running");
        FfiReseedResult result;
        try
        {
            result = await _nest.ReseedCustodianStoreAsync(channel, _deviceId);
        }
        catch (Exception ex)
        {
            result = new FfiReseedResult(
                @stopped: ex is FfiException.General g ? g.msg : ex.Message,
                @resultLines: Array.Empty<uniffi.fauna_core.LocalizedText>(),
                @isWhole: false, @reenrollError: null);
        }
        finally
        {
            ReseedRunning = false;
        }

        if (result.@stopped is { } reason)
        {
            ReseedResultText = null;
            SetError(Strings.Format("backups/backup_reseed_failed", reason));
            return false;
        }
        // Each line resolved NESTED: a set line's {set} is itself a key.
        ReseedResultText = string.Join("\n", result.@resultLines.Select(Strings.ResolveNested));
        if (result.@reenrollError is { } reenroll)
        {
            // The data IS back; the result says so, and this says what did not happen.
            SetError(Strings.Format("backups/backup_reseed_reenroll_failed", reenroll));
            return false;
        }
        // Nothing left to say: re-list, so the re-enrolled row appears and the
        // orphaned verdict is re-measured against it.
        await LoadDestinationsAsync();
        return true;
    }

    /// <summary>Whether <c>backup-sole-client-destination-warning</c> should
    /// render: every CONFIGURED destination is a client device (an owner with
    /// only device-backed copies has no true off-site backup). Reads the RAW
    /// <c>FfiBackupDestinationView</c> list via the shared
    /// <c>every_destination_is_a_client_device</c> predicate — a row of an
    /// unrecognised kind counts as NOT a client device (the conservative
    /// direction), which is exactly what re-deriving this locally would risk
    /// getting backwards.</summary>
    public bool SoleClientWarningVisible =>
        FaunaFfiMethods.EveryDestinationIsAClientDevice(_destinationViews.ToArray());

    /// Map a destination-mutation failure: the FFI's stable different-nest
    /// sentinel becomes the localized hint; everything else uses the shared
    /// error formatter.
    private void MapDestinationError(Exception ex)
    {
        if (ex is FfiException.General g && g.msg == EditDifferentNestToken)
            SetError(Strings.Get("backups/backup_destination_edit_different_nest"));
        else
            ShowError(ex);
    }

    /// <summary>Read the LIVE per-destination status and rebuild the <c>_statuses</c> map
    /// keyed by destination_id (backups.md § Per-destination status read). Skipped at zero
    /// destinations (the FFI fn also short-circuits zero destinations) — no
    /// <c>_deviceId</c> half: the nest derives the owner from the authenticated
    /// connection, so a stable sync device id was never a precondition of this
    /// particular read (only of the other, device-scoped calls this VM still makes
    /// directly), matching apple's <c>refreshStatuses</c> guard
    /// (<c>BackupDestinationsVM.swift</c>, destination count only). A transient
    /// failure leaves the prior map — the rows degrade to the "never"/"0 queued"
    /// baseline rather than showing a scary error before the user acts (mirrors
    /// apple <c>refreshStatuses</c> / linux <c>run_status_read</c>). Run from
    /// <see cref="LoadDestinationsAsync"/> + after every add/edit/remove, so the
    /// rows re-read on page mount and after each mutation.</summary>
    public async Task RefreshStatusesAsync()
    {
        if (Destinations.Count == 0)
        {
            _statuses = new Dictionary<string, FfiBackupDestinationStatus>();
            return;
        }
        try
        {
            // One read for every app since the leg-(d) repoint: the nest's
            // fauna.backup.status projection (backups.md § Per-destination status
            // read). The fold onto the always-on upload driver's long-lived handle is
            // gone — there is no local segment-backup.sqlite behind this page any
            // more, and the nest's numbers are live with the app closed. No
            // ConfigureAwait(false) — WinUI bound-state rule (mutating _statuses must
            // resume on the UI thread).
            var rows = await _nest.BackupDestinationStatusAsync();
            _statuses = rows.ToDictionary(r => r.destinationId);
        }
        catch
        {
            // Degrade to baseline; no scary error before the user acts.
        }
    }

    /// <summary>Run the client-side audit pass and rebuild the <c>_auditRows</c> map
    /// keyed by destination_id, then the <see cref="AuditAlerts"/> banner list
    /// (backups.md § Audit-alert surface). Skipped at zero destinations or before
    /// the actor-scoped audit-state path is known — <c>backup_audit_run_pass</c>
    /// reads the destination set server-side, so this is purely a round-trip-saving
    /// gate, mirroring <see cref="RefreshStatusesAsync"/>. A transient failure
    /// leaves the prior map/banners (degrade to last-known state, no scary error
    /// before the user acts). Deliberately a SEPARATE call from
    /// <see cref="RefreshStatusesAsync"/> (linux's <c>refresh_audit</c> shape): a
    /// slow/unreachable destination must not block the primary status-row read.
    /// Run from <see cref="LoadDestinationsAsync"/> + after every add/edit/remove,
    /// and from <c>backup_audit_run_now</c> (TestAgent) to force a re-pass.</summary>
    public async Task RefreshAuditAsync()
    {
        if (Destinations.Count == 0 || string.IsNullOrEmpty(_auditStatePath))
        {
            _auditRows = new Dictionary<string, FfiDestinationAuditRow>();
            AuditAlerts.Clear();
            return;
        }
        try
        {
            var rows = await _nest.BackupAuditRunPassAsync(_auditStatePath, _syncStateDir);
            _auditRows = rows.ToDictionary(r => r.destinationId);
            RebuildAuditAlerts();
        }
        catch
        {
            // Degrade to the prior state; no scary error before the user acts.
        }
    }

    /// Rebuild <see cref="AuditAlerts"/> from <c>_auditRows</c> PLUS a
    /// client-device custodian's own reported failure (backups.md § Audit-alert
    /// surface → *The client-device arm*): one localized banner per destination
    /// currently failing (<c>alert_reason != null</c>), in destination-row order,
    /// THEN one per client-device row whose own last-reported audit state is
    /// alerting via the shared <c>backup_self_audit_is_alerting</c> predicate —
    /// the only failure signal that exists for a kind the owner-side loop can
    /// never sample. Appended as a SEPARATE pass, not folded into the same loop —
    /// a destination could in principle carry both an owner-side and a
    /// self-reported reason, and <see cref="BackupAuditAlertReason.SelfReported"/>
    /// is a distinct reason from whatever <c>_auditRows</c> holds for it.
    /// Destinations with no audit yet, or a passing one, contribute nothing —
    /// mirrors linux <c>render_alerts</c> / apple's flat <c>auditAlerts</c> text list.
    private void RebuildAuditAlerts()
    {
        AuditAlerts.Clear();
        foreach (var d in Destinations)
        {
            if (_auditRows.TryGetValue(d.Id, out var row) && row.alertReason is { } reason)
            {
                AuditAlerts.Add(Strings.Resolve(FaunaFfiMethods.BackupAuditAlertLabel(reason, d.Label)));
            }
        }
        foreach (var d in Destinations)
        {
            if (RowIsAClientDevice(d.Id) &&
                FaunaFfiMethods.BackupSelfAuditIsAlerting(_statuses.GetValueOrDefault(d.Id)?.auditState))
            {
                AuditAlerts.Add(Strings.Resolve(
                    FaunaFfiMethods.BackupAuditAlertLabel(new BackupAuditAlertReason.SelfReported(), d.Label)));
            }
        }
    }

    /// <summary>The <c>backup-destination-last-audit-time</c> CELL text for a row,
    /// dispatched on the row's kind (backups.md § Audit-alert surface → *The
    /// client-device arm*; mirrors linux's <c>audit_cell_text</c> / apple's
    /// <c>auditCellText</c>): a client-device custodian carries no address for
    /// this client's own audit pass to reach, so it reads its own last-passed
    /// self-audit instead of the owner-side loop's structural silence; every
    /// other kind keeps this client's independent audit-loop verdict. Reads the
    /// shared <c>destination_row_is_a_client_device</c> predicate via
    /// <see cref="RowIsAClientDevice"/>, never a hand-rolled kind compare.</summary>
    public string AuditCellText(string destinationId) =>
        RowIsAClientDevice(destinationId) ? SelfAuditText(destinationId) : LastAuditText(destinationId);

    /// <summary>The <c>backup-destination-last-audit-time</c> text for a
    /// CLIENT-DEVICE custodian row — the custodian's own last PASSED self-audit,
    /// via the separate shared door <c>backup_self_audit_label</c> (backups.md §
    /// Audit-alert surface → *The client-device arm*): the owner-side loop and a
    /// custodian's self-report answer the same question from opposite sides of
    /// the trust line. Reads the STATUS row's <c>lastAuditPassedAt</c>, never
    /// <c>_auditRows</c> — a custodian has no address for this client's own audit
    /// pass to reach, so it carries no entry there.</summary>
    public string SelfAuditText(string destinationId) =>
        SelfAuditText(_statuses.GetValueOrDefault(destinationId),
                       DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());

    /// Pure status → i18n text mapping, through the SEPARATE self-audit door —
    /// never the owner-side wording, so a self-report can never wear the
    /// authority of an independent verification. null / <c>lastAuditPassedAt</c>
    /// == null renders absence ("Self-checked: not yet"), never a verdict — the
    /// same refusal the nest makes for a row that has not reported. Static + pure
    /// so unit tests pin the mapping without a live FFI read (<paramref
    /// name="nowMs"/> injected for determinism). Mirrors
    /// <see cref="LastAuditText(FfiDestinationAuditRow?, long)"/>.
    internal static string SelfAuditText(FfiBackupDestinationStatus? status, long nowMs)
    {
        var display = FaunaFfiMethods.BackupSelfAuditLabel(status?.lastAuditPassedAt, nowMs);
        var text = Strings.Resolve(display.label);
        if (display.when is { } when)
        {
            text = text.Replace("{when}", ValueFormat.ResolveRelativeTimeDisplay(when, nowMs));
        }
        return text;
    }

    /// <summary>The <c>backup-destination-last-audit-time</c> text for a row: the
    /// live audit row's <c>last_passed_at</c> through the shared relative-time
    /// formatter, else the "never" baseline (no passed audit yet, or no read yet).
    /// Mirrors <see cref="LastUploadText(string)"/>. The owner-side half of
    /// <see cref="AuditCellText"/>'s dispatch — a client-device row never reaches
    /// this (see <see cref="SelfAuditText(string)"/>).</summary>
    public string LastAuditText(string destinationId) =>
        LastAuditText(_auditRows.GetValueOrDefault(destinationId),
                      DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());

    /// Pure audit row → i18n text mapping. null / last_passed_at == null ⇒ the
    /// "never" baseline; a real timestamp (unix seconds) renders through the
    /// shared <see cref="ValueFormat.RelativeTime"/> (epoch ms). Static + pure so
    /// unit tests pin the mapping without a live FFI read (<paramref name="nowMs"/>
    /// injected for determinism). Mirrors <see cref="LastUploadText(FfiBackupDestinationStatus?, long)"/>.
    internal static string LastAuditText(FfiDestinationAuditRow? row, long nowMs)
    {
        var display = FaunaFfiMethods.BackupLastAuditLabel(row?.lastPassedAt, nowMs);
        var text = Strings.Resolve(display.label);
        if (display.when is { } when)
        {
            text = text.Replace("{when}", ValueFormat.ResolveRelativeTimeDisplay(when, nowMs));
        }
        return text;
    }

    /// <summary>e2e-only: force a fresh audit pass after applying a clock offset
    /// (<c>backup_audit_run_now</c>, TestAgent — backups.md § Audit-alert surface).
    /// Mirrors linux's <c>set_clock_offset_secs</c> + <c>poke_rerun</c>. The offset
    /// is a process-wide FFI static that nothing auto-resets — the caller (the e2e
    /// test) is responsible for zeroing it back when done.
    ///
    /// <para>Compile-gated (convention 15). The FFI seam it calls is exported only
    /// under `test-helpers` (`fauna-ffi/src/backup_destinations.rs` — see its own
    /// cfg), so in a production flavor this method does not compile at all: it was
    /// ungated plumbing over a correctly-gated seam, which is the exact shape the
    /// convention's "same-signature no-op twin wherever the caller is plumbing"
    /// clause covers. No twin is needed here — the sole caller, `TestAgent`, is
    /// itself behind the same gate. ⚠ This broke `just windows-release` outright
    /// and went unnoticed because NO merge gate builds Release: both
    /// `windows-cs-test-compile` (`dotnet test`) and `windows-app-build`
    /// (`just windows-debug`) are Debug-only.</para></summary>
#if DEBUG || FAUNA_E2E_AGENT
    public async Task RunAuditNowAsync(long nowOffsetSecs)
    {
        FaunaFfiMethods.SetBackupAuditClockOffsetSecs(nowOffsetSecs);
        await RefreshAuditAsync();
    }
#endif

    /// <summary>The <c>backup-destination-last-upload-time</c> text for a row: the live
    /// status's <c>last_upload_time</c> through the shared relative-time formatter, else
    /// the "never" baseline (no upload yet, or no read yet). Mirrors apple
    /// <c>lastUploadText(for:)</c>.</summary>
    public string LastUploadText(string destinationId) =>
        LastUploadText(_statuses.GetValueOrDefault(destinationId),
                       DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());

    /// <summary>The <c>backup-destination-backlog-count</c> text for a row ("N queued"),
    /// 0 when there is no read yet. Mirrors apple <c>backlogText(for:)</c>.</summary>
    public string BacklogText(string destinationId) =>
        BacklogText(_statuses.GetValueOrDefault(destinationId));

    /// Pure status → i18n text mapping (the client-glue half of backups.md
    /// § Per-destination status read). null / last_upload_time == null ⇒ the "never"
    /// baseline; a real timestamp (unix seconds) renders through the shared
    /// <see cref="ValueFormat.RelativeTime"/> (epoch ms). Static + pure so the unit tests
    /// pin the mapping without a live FFI read (<paramref name="nowMs"/> injected for
    /// determinism). Mirrors apple <c>BackupDestinationsVM.lastUploadText</c>.
    internal static string LastUploadText(FfiBackupDestinationStatus? status, long nowMs)
    {
        var display = FaunaFfiMethods.BackupLastUploadLabel(status?.lastUploadTime, nowMs);
        var text = Strings.Resolve(display.label);
        if (display.when is { } when)
        {
            text = text.Replace("{when}", ValueFormat.ResolveRelativeTimeDisplay(when, nowMs));
        }
        return text;
    }

    /// Pure backlog-count → i18n mapping. null ⇒ "0 queued". Mirrors apple
    /// <c>BackupDestinationsVM.backlogText</c>.
    internal static string BacklogText(FfiBackupDestinationStatus? status) =>
        Strings.Resolve(FaunaFfiMethods.BackupBacklogLabel(status?.backlogCount));

    // ── Restore surface (backups.md §§ Restore history / divergence / from destination) ──
    // Thin glue over the shared FfiSnapshotsClient restore reads + the local
    // restore action (backups.md § Where logic lives — composition is shared
    // Rust; the render rules below are i18n-bound client glue mirroring
    // android/linux). The cross-location destination pull is fauna-sync Plan 4
    // (pending); the wired path is the local single-snapshot restore.

    /// The forensic restore-history rows (read side) — one indexed
    /// <c>restore-history-item</c> each, with its divergence banner + details.
    public ObservableCollection<RestoreHistoryRow> RestoreHistory { get; } = new();

    /// The local message-kind snapshots feeding <c>restore-snapshot-select</c>.
    public ObservableCollection<RestoreSnapshotOption> RestoreSnapshots { get; } = new();

    /// Whether ≥1 backup destination is configured — gates the (disabled-at-zero)
    /// <c>restore-source-select</c> picker (cross-location pull is Plan 4).
    [ObservableProperty] private bool _hasDestinations;

    /// The picker selection; the friction bar matches the typed id against this.
    [ObservableProperty] private RestoreSnapshotOption? _selectedRestoreSnapshot;

    /// The <c>restore-confirm-input</c> friction-bar text (re-typed snapshot id).
    [ObservableProperty] private string _restoreConfirmInput = "";

    /// <c>restore-confirm-button</c> enabled state: a snapshot selected AND the
    /// typed id matches it AND no restore in flight.
    [ObservableProperty] private bool _restoreConfirmEnabled;

    /// True while a restore is dispatching (disables the confirm button).
    [ObservableProperty] private bool _restoreInProgress;

    /// <c>restore-progress</c> step text (idle / running / done).
    [ObservableProperty] private string _restoreProgressText = Strings.Get("backups/restore_progress_idle");

    /// The reply <c>note</c> when <c>config_present == false</c> (the bridge can't
    /// AUTH until the wrapped-MLS blob bundle is restored too); null otherwise.
    [ObservableProperty] private string? _restoreWarning;

    partial void OnRestoreConfirmInputChanged(string value) => RecomputeRestoreEnabled();
    partial void OnSelectedRestoreSnapshotChanged(RestoreSnapshotOption? value) => RecomputeRestoreEnabled();
    partial void OnRestoreInProgressChanged(bool value) => RecomputeRestoreEnabled();

    private void RecomputeRestoreEnabled()
    {
        RestoreConfirmEnabled =
            SelectedRestoreSnapshot is not null
            && !string.IsNullOrEmpty(RestoreConfirmInput)
            && RestoreConfirmInput == SelectedRestoreSnapshot.Id.ToString()
            && !RestoreInProgress;
    }

    /// <summary>Hydrate the whole restore surface: the history rows (+ per-row
    /// forensic divergence), the local snapshot picker, and the destination-gate.
    /// Each read degrades independently so one failure doesn't blank the rest.</summary>
    public async Task LoadRestoreAsync()
    {
        ErrorMessage = null;
        await LoadRestoreHistoryAsync();

        // Local snapshot picker (the wired restore source).
        try
        {
            var snaps = await _nest.MessageKindSnapshotListAsync();
            RestoreSnapshots.Clear();
            foreach (var s in snaps)
                RestoreSnapshots.Add(new RestoreSnapshotOption(s.id, SnapshotLabel(s)));
            // Auto-select the first so the friction bar has a target (the picker
            // defaults to the listed snapshot; mirrors android/linux).
            SelectedRestoreSnapshot = RestoreSnapshots.Count > 0 ? RestoreSnapshots[0] : null;
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }

        // restore-source-select is disabled at zero destinations (cross-location
        // pull is fauna-sync Plan 4); a failure degrades to "no destinations".
        try
        {
            HasDestinations = (await _nest.BackupDestinationsListAsync()).Count > 0;
        }
        catch
        {
            HasDestinations = false;
        }
    }

    /// Load (or reload) just the restore-history rows + their forensic divergence.
    /// Shared by <see cref="LoadRestoreAsync"/> and the post-restore refresh.
    private async Task LoadRestoreHistoryAsync()
    {
        try
        {
            var rows = await _nest.SnapshotListRestoreHistoryAsync();
            RestoreHistory.Clear();
            foreach (var r in rows)
                RestoreHistory.Add(await BuildRestoreHistoryRowAsync(r));
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// Project one FFI restore-history row into its display row, fetching the
    /// forensic divergence for it (a per-row fetch failure degrades to "no banner"
    /// rather than failing the whole load — mirrors the android lead).
    private async Task<RestoreHistoryRow> BuildRestoreHistoryRowAsync(FfiRestoreHistoryRow r)
    {
        bool isLocal = r.sourceMemberId is null;
        string source = isLocal
            ? Strings.Get("backups/restore_source_local")
            : FaunaFfiMethods.HexShort(r.sourceMemberId!);
        string description = Strings.Get("backups/restore_history_row")
            .Replace("{kinds}", r.kindsRestored)
            .Replace("{source}", source)
            .Replace("{when}", FormatEpochSeconds(r.completedAt));

        IReadOnlyList<FfiRestoreDivergenceRow> divergence;
        try
        {
            divergence = await _nest.SnapshotListRestoreDivergenceAsync(r.snapshotId);
        }
        catch
        {
            divergence = new List<FfiRestoreDivergenceRow>();
        }

        var details = new List<RestoreDivergenceDetailRow>(divergence.Count);
        foreach (var d in divergence)
            details.Add(new RestoreDivergenceDetailRow(FormatDivergenceDetail(d)));

        string banner = Strings.Get("backups/restore_divergence_banner")
            .Replace("{count}", divergence.Count.ToString());

        return new RestoreHistoryRow(
            r.snapshotId, description, isLocal, divergence.Count > 0, banner, details);
    }

    /// <summary>Dispatch the local single-snapshot restore for the selected
    /// snapshot (the friction bar guarantees the typed id matches). Surfaces the
    /// reply warning when the wrapped-MLS blob bundle isn't present, then reloads the history.</summary>
    public async Task RestoreAsync()
    {
        if (SelectedRestoreSnapshot is null) return;

        RestoreInProgress = true;
        RestoreWarning = null;
        ErrorMessage = null;
        RestoreProgressText = Strings.Get("backups/restore_progress_running");
        try
        {
            var reply = await _nest.SnapshotRestoreMessageKindAsync(
                SelectedRestoreSnapshot.Id, RestoreConfirmInput);
            RestoreProgressText = Strings.Get("backups/restore_progress_done");
            // config_present == false → the bridge can't AUTH until the wrapped-MLS blob bundle is
            // restored too; surface the reply note as a warning (backups.md).
            if (!reply.configPresent && !string.IsNullOrEmpty(reply.note))
                RestoreWarning = reply.note;
            // The restore wrote a restore_history row — reflect it.
            await LoadRestoreHistoryAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
            RestoreProgressText = Strings.Get("backups/restore_progress_idle");
        }
        finally
        {
            RestoreInProgress = false;
        }
    }

    /// "{kind} (#{id})" via the shared `snapshotRestoreOptionLabel` — matches
    /// linux/android/tui/apple, which consume the same logic.
    private static string SnapshotLabel(FfiSnapshotSummary s) =>
        FaunaFfiMethods.SnapshotRestoreOptionLabel(s.messageKind, s.id);

    /// "{collection} · {mua} · client modseq {client} / server modseq {server} ·
    /// ~{lost} writes lost"; mua None → "(unknown)" (backups.md § Restore divergence).
    private string FormatDivergenceDetail(FfiRestoreDivergenceRow d) =>
        Strings.Get("backups/restore_divergence_detail_row")
            .Replace("{collection}", d.collection)
            .Replace("{mua}", d.muaId ?? Strings.Get("backups/restore_divergence_unknown_mua"))
            .Replace("{client}", d.clientModseq.ToString())
            .Replace("{server}", d.serverModseq.ToString())
            .Replace("{lost}", d.lostEventCount.ToString());

    // ── Immediate-delete modal (backups.md § User actions + the behavioural
    //    invariant, line 309: never a one-click affordance — the confirm enables
    //    ONLY when BOTH friction inputs match exactly) ────────────────────────
    // Owner-only immediate (skip-soft-delete) snapshot removal. The per-row
    // snapshot-immediate-delete-button OPENS this modal; the nest re-checks both
    // inputs + the hard floor (>3 active) + owner-only. Mirrors the RestoreConfirm
    // friction bar with two matched inputs instead of one.

    /// Whether the immediate-delete confirmation modal is open.
    [ObservableProperty] private bool _immediateDeleteOpen;

    /// The snapshot the modal targets (set when the row button opens it).
    [ObservableProperty] private long _immediateDeleteSnapshotId;

    /// <c>immediate-delete-confirm-input</c> — the user re-types the snapshot id.
    [ObservableProperty] private string _immediateDeleteConfirmInput = "";

    /// <c>immediate-delete-acknowledge-input</c> — the user types the exact
    /// acknowledge string (<see cref="ImmediateDeleteAckText"/>).
    [ObservableProperty] private string _immediateDeleteAcknowledgeInput = "";

    /// <c>immediate-delete-confirm-button</c> enabled state — the SHARED predicate,
    /// called (see <see cref="RecomputeImmediateDeleteEnabled"/>), never re-derived
    /// here.
    [ObservableProperty] private bool _immediateDeleteEnabled;

    /// The exact acknowledge string the user must type, surfaced from the shared
    /// FFI (the SAME constant the nest checks byte-for-byte) so the displayed +
    /// validated string can never drift from the server's <c>acknowledge_mismatch</c>
    /// gate. Lazily fetched so tests that never open the modal need no native call.
    public string ImmediateDeleteAckText => _ackText ??= FaunaFfiMethods.ImmediateDeleteAckText();
    private string? _ackText;

    partial void OnImmediateDeleteConfirmInputChanged(string value) => RecomputeImmediateDeleteEnabled();
    partial void OnImmediateDeleteAcknowledgeInputChanged(string value) => RecomputeImmediateDeleteEnabled();

    /// The enable predicate is the machine's
    /// <c>fauna_client_snapshots::immediate_delete_button_enabled</c> with its REAL
    /// in-flight flag threaded in — windows was the last hand-rolled copy of the
    /// four-way <c>&amp;&amp;</c>, and the <c>deleting</c> half is the one every app
    /// got wrong. With no machine attached the button stays disabled: the friction
    /// bar fails CLOSED.
    private void RecomputeImmediateDeleteEnabled() =>
        ImmediateDeleteEnabled = _machine is not null && _machine.ImmediateDeleteEnabled(
            ImmediateDeleteConfirmInput,
            ImmediateDeleteSnapshotId.ToString(),
            ImmediateDeleteAcknowledgeInput);

    /// <summary>Open the immediate-delete modal for <paramref name="snapshotId"/>,
    /// clearing both friction inputs (so the confirm starts disabled).</summary>
    public void OpenImmediateDelete(long snapshotId)
    {
        ImmediateDeleteSnapshotId = snapshotId;
        ImmediateDeleteConfirmInput = "";
        ImmediateDeleteAcknowledgeInput = "";
        ErrorMessage = null;
        ImmediateDeleteOpen = true;
        RecomputeImmediateDeleteEnabled();
    }

    /// <summary>Close the modal with no side effect (<c>immediate-delete-cancel-button</c>).</summary>
    public void CancelImmediateDelete() => ImmediateDeleteOpen = false;

    /// <summary>Dispatch the owner-only immediate delete. The machine re-checks the
    /// friction bar rather than trusting this caller, so a client that wires the
    /// button wrong still cannot skip it (Architectural rule 4 is a behavioural
    /// invariant, not styling).
    ///
    /// <para>The modal closes on the row LEAVING the machine's list — never on
    /// this call returning. A <c>hard_floor_breach</c> refusal returns from the
    /// same call and must leave the modal and the typed inputs standing (linux's
    /// lesson, inherited).</para></summary>
    public async Task ConfirmImmediateDeleteAsync()
    {
        if (_machine is null || !ImmediateDeleteEnabled) return;
        var target = ImmediateDeleteSnapshotId;
        await DispatchAsync(m => m.DeleteSnapshotImmediate(
            target, ImmediateDeleteConfirmInput, ImmediateDeleteAcknowledgeInput));
        CloseImmediateDeleteIfLanded(target);
    }

    /// Close the friction-bar modal once the row it targets has actually left the
    /// machine's list. An in-flight op or a live error both mean "not landed", so
    /// neither closes it.
    private void CloseImmediateDeleteIfLanded(long target)
    {
        if (IsBusy || !string.IsNullOrEmpty(ErrorMessage)) return;
        if (Snapshots.Any(s => s.Id == target)) return;
        ImmediateDeleteOpen = false;
    }
}
