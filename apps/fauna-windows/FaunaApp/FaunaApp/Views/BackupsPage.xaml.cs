using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// The Backups page. Its SNAPSHOT half renders off the shared-Rust
/// <c>fauna-backups-machine</c> (built over the one UniFFI face
/// <c>build_backups_machine</c> and held by <see cref="BackupsViewModel"/>): this
/// class paints <c>backups_snapshot()</c> and dispatches gestures, and holds no
/// page logic (<c>ui/backups.md</c> § Snapshot-list shape, Architectural rule 1).
/// The destination + restore halves keep their own seams.
/// </summary>
public sealed partial class BackupsPage : Page
{
    /// <summary>x:Bind converter for the per-file download affordance — WinUI has
    /// no built-in bool→Visibility binding. Qualified as
    /// <c>local:BackupsPage.BoolToVisibility</c> at the call site: an unqualified
    /// static x:Bind method fails the XAML compiler with CS0176.</summary>
    public static Visibility BoolToVisibility(bool value) =>
        value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>The live page instance, for TestAgent's <c>backup_audit_run_now</c>
    /// command (linux's <c>set_rerun_hook</c>/<c>poke_rerun</c> idiom, adapted: the
    /// page installs itself while live and clears on navigate-away, so a command
    /// with no page mounted fails loudly instead of silently dropping — convention
    /// 11). Mirrors <c>ConversationsPage.Current</c>/<c>ProfilePage.Current</c>.</summary>
    internal static BackupsPage? Current { get; private set; }

    private BackupsViewModel? _viewModel;

    /// The session's WS-RPC client + this device's stable sync device id, kept from
    /// OnNavigatedTo so Page_Loaded can build the snapshot-half machine (the build
    /// needs a CONNECTED client, so it cannot happen in OnNavigatedTo).
    private Core.Services.INestRpcClient? _rpc;
    private string _deviceIdHex = "";

    /// Keeps the machine's observer alive for the page's lifetime — UniFFI holds it
    /// by a foreign handle, so a collected observer would silently stop ticking.
    private Sync.BackupsNotifyObserver? _observer;

    // Backup-destinations dialog state: the destination_id the add/edit dialog is
    // editing (null while adding), and the id the remove-confirm dialog is armed for.
    private string? _editingDestinationId;
    private string? _pendingRemoveId;

    // The currently-rendered destination rows, keyed and ordered by destination
    // id — lets RenderDestinations RECONCILE instead of rebuilding when the id
    // set is unchanged (see RenderDestinations).
    private readonly List<string> _destinationRowIds = new();
    private readonly Dictionary<string, DestinationRowElements> _destinationRowsById = new();

    public BackupsPage()
    {
        this.InitializeComponent();
        BackupsTitle.Text = S.Get("backups/title");
        BackupNowButton.Content = S.Get("backups/backup_now");
        // "Apply retention policy", not "Prune": the button applies THIS SET'S OWN
        // resting policy (preview first), and never carries a client-chosen one
        // (backups.md § Snapshot-list shape → *Prune*, Architectural rule 5).
        PruneButton.Content = S.Get("backups/prune_button");
        CheckButton.Content = S.Get("backups/check_button");
        PrunePreviewTitle.Text = S.Get("backups/prune_preview_title");
        PruneExecuteButton.Content = S.Get("backups/prune_execute_button");
        PruneCancelButton.Content = S.Get("backups/prune_cancel_button");
        // The ordinary-delete confirm (client glue; windows shipped none before).
        DeleteSnapshotDialog.Title = S.Get("backups/delete_snapshot");
        DeleteSnapshotText.Text = S.Get("backups/delete_snapshot_confirm");
        DeleteSnapshotDialog.PrimaryButtonText = S.Get("backups/delete_snapshot");
        DeleteSnapshotDialog.CloseButtonText = S.Get("backups/prune_cancel_button");
        DeleteSnapshotDialog.DefaultButton = ContentDialogButton.Close;
        // (The picker's items are the folders themselves — filled on load by
        // PopulateFolderSelector; a ComboBox has no static Content.)
        EmptyText.Text = S.Get("backups/no_snapshots");
        NoFilesText.Text = S.Get("backups/select_snapshot");
        // Reconcile the detail pane to its no-selection state up front. Both
        // FileList and NoFilesText default to Visible in XAML and share Row 1, so
        // without this they render on top of each other until the first detail
        // opens — and an empty-but-Visible `snapshot-detail-files` also lets an
        // e2e "wait for the detail to open" pass before anything opened.
        //
        // Set directly rather than via UpdateFileList: that reads the VM, which
        // does not exist yet at construction (OnNavigatedTo builds it), so calling
        // it here would silently do nothing and leave both halves visible.
        DetailHeader.Text = S.Get("backups/select_snapshot");
        FileList.Visibility = Visibility.Collapsed;
        NoFilesText.Visibility = Visibility.Visible;

        // Backup destinations (management) — static labels.
        DestinationsHeader.Text = S.Get("backups/backup_destinations_title");
        DestinationsDesc.Text = S.Get("backups/backup_destinations_desc");
        DestinationsEmpty.Text = S.Get("backups/backup_destinations_empty");
        DestinationAddButton.Content = S.Get("backups/backup_destination_add_button");
        DestinationUrlBox.PlaceholderText = S.Get("backups/backup_destination_url_placeholder");
        DestinationNameBox.PlaceholderText = S.Get("backups/backup_destination_name_placeholder");
        DestinationConfirmButton.Content = S.Get("backups/backup_destination_add_confirm");
        DestinationCancelButton.Content = S.Get("backups/backup_destination_add_cancel");
        DestinationRemoveText.Text = S.Get("backups/backup_destination_remove_confirm_title");
        DestinationRemoveConfirmButton.Content = S.Get("backups/backup_destination_remove_confirm_button");
        DestinationRemoveCancelButton.Content = S.Get("backups/backup_destination_remove_cancel_button");
        // Removing a destination tears down at the destination nest AND the source
        // nest's registry, so it genuinely needs a nest (W4 (account-data-plane.md § Workstreams) phase 4). Cancel is
        // pure local UI and declares nothing — it must stay live in the same modal,
        // at the same moment, as the gated confirm beside it.
        DestinationRemoveConfirmButton.FaunaGate("fauna.backup.destination.remove");
        DestinationCapacityBox.PlaceholderText = S.Get("backups/backup_destination_capacity_placeholder");
        DestinationCustodianExposureNote.Text = S.Get("backups/backup_destination_custodian_exposure");
        PopulateDestinationKindSelect();

        // Restore surface (backups.md §§ Restore history / divergence / from destination).
        RestoreHistoryHeader.Text = S.Get("backups/restore_section_title");
        RestoreLocalHeader.Text = S.Get("backups/restore_local_title");
        RestoreSourceSelect.PlaceholderText = S.Get("backups/backup_destinations_empty");
        RestoreSnapshotSelect.PlaceholderText = S.Get("backups/restore_no_snapshots");
        RestoreKindMail.Content = S.Get("backups/restore_kinds_mail");
        RestoreKindCalendar.Content = S.Get("backups/restore_kinds_calendar");
        RestoreConfirmInput.PlaceholderText = S.Get("backups/restore_confirm_placeholder");
        RestoreConfirmButton.Content = S.Get("backups/restore_confirm_button");
        RestoreProgress.Text = S.Get("backups/restore_progress_idle");
        DivergenceTitle.Text = S.Get("backups/restore_divergence_modal_title");
        DivergenceFooter.Text = S.Get("backups/restore_divergence_footer");
        DivergenceCloseButton.Content = S.Get("backups/restore_divergence_close");

        // Immediate-delete modal — static labels; the title + warning {id} +
        // acknowledge phrase are set per-open in ImmediateDelete_Click.
        ImmediateDeleteConfirmInput.PlaceholderText = S.Get("backups/immediate_delete_confirm_id_placeholder");
        ImmediateDeleteAcknowledgeInput.PlaceholderText = S.Get("backups/immediate_delete_acknowledge_placeholder");
        ImmediateDeleteAckPrompt.Text = S.Get("backups/immediate_delete_acknowledge_prompt");
        ImmediateDeleteConfirmButton.Content = S.Get("backups/immediate_delete_confirm_button");
        ImmediateDeleteCancelButton.Content = S.Get("backups/immediate_delete_cancel_button");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            // Plumb device_id = the stable sync device id (the SAME id the file-sync
            // engine uses) — this page's own device-scoped calls still need it
            // (single-file restore, client-device custodian enrollment). The
            // per-destination status read itself no longer does (backups.md §
            // Per-destination status read: the nest derives the owner from the
            // authenticated connection; the retired `(device_id, data_dir)`
            // canonical-path contract and the always-on upload driver it belonged to
            // are both gone).
            // Single-file restore (snapshot-file-download-button): the byte fetch
            // rides the HTTP residue client; the save step goes through the
            // ISnapshotFileSaver seam — the native FileSavePicker in production,
            // a fixed directory under FAUNA_E2E_BRIDGE (a native save dialog is
            // not e2e-driveable; the windows e2e driver's download_dir() mirrors
            // this directory choice).
            // Both reads go through E2eEnv so they are compiled out of release
            // builds (convention 15); the production twins return null, so the
            // shipped app always takes the native FilePicker arm, as before.
            Core.Services.ISnapshotFileSaver fileSaver = Services.SnapshotFileSavers.ForSession();
            // The audit loop's own state file, actor-scoped (backups.md § Audit-alert
            // surface: two accounts sharing one path would let one account's
            // observation high-water suppress the other's freshness failures).
            var actorIdHex = clients.Crypto is { HasKey: true } cc ? cc.ActorIdHex : null;
            _rpc = clients.Rpc;
            _deviceIdHex = clients.Account.DeviceId ?? "";
            _viewModel = new BackupsViewModel(
                clients.Rpc!, clients.Account.DeviceId ?? "",
                fileSaver: fileSaver,
                auditStatePath: Core.Services.AccountStateDir.BackupAuditStatePath(actorIdHex),
                // Resolved per call, never captured — the agent session is rebuilt
                // on every login and nest re-point, so a captured channel would pin
                // a dead handle (the AgentStatusProbe / AgentSyncNudge idiom). Null
                // when this device drives no agent, which the VM reads as
                // "not orphaned" — the conservative direction for a gesture that
                // deletes the owner's only offline copy.
                agentChannel: () => App.CurrentSyncAgent?.Channel,
                // The covered-folder audit's population anchor: the agent's own
                // replica of each bound folder (windows runs no in-app engine).
                syncStateDir: Core.Services.AccountStateDir.SyncAgentStateDir(actorIdHex));
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
            Current = this;
        }
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        if (ReferenceEquals(Current, this)) Current = null;
    }

    /// <summary>e2e-only: apply an audit clock offset, force a re-pass, and
    /// re-render the destination rows + alert banners — <c>backup_audit_run_now</c>
    /// (TestAgent). See <see cref="BackupsViewModel.RunAuditNowAsync"/>.
    /// Compile-gated for the same reason as the VM method it forwards to
    /// (convention 15): the underlying FFI seam is `test-helpers`-only.</summary>
#if DEBUG || FAUNA_E2E_AGENT
    internal async Task RunAuditNowAsync(long nowOffsetSecs)
    {
        if (_viewModel is null) return;
        await _viewModel.RunAuditNowAsync(nowOffsetSecs);
        RenderDestinations();
    }
#endif

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;

        // Build the snapshot-half machine bound to the session WS-RPC requester and
        // hand it to the VM; the observer ticks the UI thread → RenderSnapshotSurface.
        // A build failure leaves the snapshot half empty and says so on the error
        // banner — the destination + restore halves below still hydrate.
        try
        {
            _observer = new Sync.BackupsNotifyObserver(OnMachineChanged);
            var machine = await _rpc!.BuildBackupsMachineAsync(_observer, _deviceIdHex);
            _viewModel.AttachMachine(machine);
            await _viewModel.LoadCommand.ExecuteAsync(null);
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Core.Services.Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }

        SnapshotList.ItemsSource = _viewModel.Snapshots;
        FileList.ItemsSource = _viewModel.DetailFiles;
        PopulateFolderSelector();

        LoadProgress.IsActive = false;
        LoadProgress.Visibility = Visibility.Collapsed;

        RefreshSnapshotSurface();

        // Backup destinations (management) — hydrate + render the status rows.
        await _viewModel.LoadDestinationsAsync();
        RenderDestinations();

        // Restore surface — hydrate history + local picker + destination gate.
        await _viewModel.LoadRestoreAsync();
        RenderRestoreHistory();
        RestoreSnapshotSelect.ItemsSource = _viewModel.RestoreSnapshots;
        if (_viewModel.RestoreSnapshots.Count > 0)
            RestoreSnapshotSelect.SelectedIndex = 0;
        RestoreSourceSelect.IsEnabled = _viewModel.HasDestinations;
        RestoreProgress.Text = _viewModel.RestoreProgressText;
    }

    private void ViewModel_PropertyChanged(object? sender,
        System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_viewModel is null) return;

        switch (e.PropertyName)
        {
            case nameof(BackupsViewModel.ErrorMessage):
                if (_viewModel.ErrorMessage is not null)
                {
                    ErrorBar.Message = _viewModel.ErrorMessage;
                    ErrorBar.IsOpen = true;
                    App.CurrentErrorMessage = _viewModel.ErrorMessage;
                }
                else
                {
                    ErrorBar.IsOpen = false;
                    App.CurrentErrorMessage = null;
                }
                break;
            case nameof(BackupsViewModel.OpenSnapshotId):
                UpdateFileList();
                break;
            case nameof(BackupsViewModel.ReseedResultText):
            case nameof(BackupsViewModel.ReseedRunning):
                RenderReseed();
                break;
            case nameof(BackupsViewModel.IsBusy):
                // Single-flight: EVERY mutating control is gated on the ONE
                // in_progress_op predicate (per-row buttons bind ActionsEnabled).
                CreateProgress.IsActive = _viewModel.IsBusy;
                CreateProgress.Visibility = _viewModel.IsBusy
                    ? Visibility.Visible : Visibility.Collapsed;
                UpdateActionButtons();
                break;
            case nameof(BackupsViewModel.SelectedFolder):
                PopulateFolderSelector();
                UpdateActionButtons();
                break;
            case nameof(BackupsViewModel.LastBackedUpText):
                LastBackedUpText.Text = _viewModel.LastBackedUpText;
                break;
            case nameof(BackupsViewModel.BusyText):
                BusyText.Text = _viewModel.BusyText ?? "";
                BusyText.Visibility = _viewModel.BusyText is null
                    ? Visibility.Collapsed : Visibility.Visible;
                break;
            case nameof(BackupsViewModel.CheckResultText):
                CheckResultText.Text = _viewModel.CheckResultText ?? "";
                CheckResultText.Visibility = _viewModel.CheckResultText is null
                    ? Visibility.Collapsed : Visibility.Visible;
                break;
            case nameof(BackupsViewModel.HasPrunePreview):
            case nameof(BackupsViewModel.PruneExecutable):
                RenderPrunePreview();
                break;
            case nameof(BackupsViewModel.RestoreConfirmEnabled):
                RestoreConfirmButton.IsEnabled = _viewModel.RestoreConfirmEnabled;
                break;
            case nameof(BackupsViewModel.RestoreProgressText):
                RestoreProgress.Text = _viewModel.RestoreProgressText;
                break;
            case nameof(BackupsViewModel.RestoreWarning):
                RestoreWarningText.Text = _viewModel.RestoreWarning ?? "";
                RestoreWarningText.Visibility = string.IsNullOrEmpty(_viewModel.RestoreWarning)
                    ? Visibility.Collapsed : Visibility.Visible;
                break;
            case nameof(BackupsViewModel.ImmediateDeleteEnabled):
                ImmediateDeleteConfirmButton.IsEnabled = _viewModel.ImmediateDeleteEnabled;
                break;
        }
    }

    /// <summary>The machine's observer tick: re-project, then repaint. One entry
    /// point, so no surface can go stale after a gesture completes elsewhere
    /// (a background refresh, or an op the user started on another control).</summary>
    private void OnMachineChanged()
    {
        _viewModel?.Apply();
        RefreshSnapshotSurface();
    }

    private async void SnapshotList_SelectionChanged(object sender,
        SelectionChangedEventArgs e)
    {
        if (_viewModel is null) return;
        if (SnapshotList.SelectedItem is SnapshotDisplayRow row)
        {
            await _viewModel.OpenSnapshotAsync(row.Id);
        }
    }

    private async void BackupNow_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.CreateSnapshotCommand.ExecuteAsync(null);
    }

    private void UpdateFileList()
    {
        if (_viewModel is null) return;
        if (_viewModel.OpenSnapshotId is not null)
        {
            DetailHeader.Text = S.Get("backups/file_count")
                .Replace("{count}", _viewModel.DetailFiles.Count.ToString());
            FileList.Visibility = Visibility.Visible;
            NoFilesText.Visibility = Visibility.Collapsed;
            // The right column's Auto rows above (restore history + local restore)
            // can grow past the viewport, leaving the outer ScrollViewer needing to
            // scroll before this pane is actually on screen.
            BringIntoViewAfterLayout(FileList);
        }
        else
        {
            DetailHeader.Text = S.Get("backups/select_snapshot");
            FileList.Visibility = Visibility.Collapsed;
            NoFilesText.Visibility = Visibility.Visible;
        }
    }

    /// <summary>Scrolls a just-revealed panel into view within its ancestor
    /// ScrollViewer. <c>UpdateLayout()</c> must run BEFORE <c>StartBringIntoView()</c>
    /// — called in the same pass as a Visibility flip, the panel has not been
    /// measured yet and has no rect to scroll to (mirrors
    /// <c>AtprotoPage.BringIntoViewAfterLayout</c>). The low-priority re-enqueue
    /// covers the animated settle after the immediate scroll.</summary>
    private void BringIntoViewAfterLayout(FrameworkElement panel)
    {
        panel.UpdateLayout();
        panel.StartBringIntoView();
        DispatcherQueue?.TryEnqueue(
            Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
            () => panel.StartBringIntoView());
    }

    private async void DownloadSnapshotFile_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn) return;
        if (btn.Tag is SnapshotFileDisplayRow file)
        {
            // Fetch + save; errors surface via ErrorMessage → ErrorBar, a
            // cancelled save dialog is a silent no-op.
            await _viewModel.DownloadSnapshotFileCommand.ExecuteAsync(file);
        }
    }

    /// <summary>Ordinary delete: CONFIRM first (client glue — backups.md
    /// § Snapshot-list shape → *Delete*; windows shipped none before this leg),
    /// then dispatch the machine gesture that queues the 48 h pending action. The
    /// row comes back as <c>DeletionPending</c>; cancelling that window is the
    /// pending-actions surface's job, not this page's.</summary>
    private async void DeleteSnapshot_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn) return;
        if (btn.Tag is SnapshotDisplayRow row)
        {
            DeleteSnapshotDialog.XamlRoot = this.XamlRoot;
            var answer = await Controls.Dialogs.ShowAsync(DeleteSnapshotDialog);
            if (answer != ContentDialogResult.Primary) return;
            await _viewModel.DeleteSnapshotCommand.ExecuteAsync(row.Id);
        }
    }

    /// <summary>Recovery is offered ONLY on a <c>SoftDeleted</c> row (the button's
    /// own <c>Recoverable</c>-gated visibility already enforces that render guard),
    /// and dispatches directly — no confirm, matching android/web/linux, none of
    /// which confirm this gesture either. The machine still refuses the call for
    /// any other row regardless of what the page renders.</summary>
    private async void UndeleteSnapshot_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn) return;
        if (btn.Tag is SnapshotDisplayRow row)
            await _viewModel.UndeleteSnapshotCommand.ExecuteAsync(row.Id);
    }

    // ── Immediate-delete modal (backups.md § User actions; the behavioural
    //    invariant line 309 — the confirm button enables only when BOTH inputs
    //    match exactly). The per-row button OPENS the modal; never one-click. ──

    private async void ImmediateDelete_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn || btn.Tag is not SnapshotDisplayRow row) return;
        var vm = _viewModel;
        ImmediateDeleteDialog.XamlRoot = this.XamlRoot;
        await Controls.Dialogs.ShowAsync(ImmediateDeleteDialog, prepare: () =>
        {
            vm.OpenImmediateDelete(row.Id);
            // Per-open labels: the title + warning carry the snapshot id; the
            // acknowledge phrase is the exact string the user must type (from the
            // shared FFI, so it can't drift from the nest's byte-for-byte check).
            ImmediateDeleteDialog.Title = S.Get("backups/immediate_delete_modal_title")
                .Replace("{id}", row.Id.ToString());
            ImmediateDeleteWarning.Text = S.Get("backups/immediate_delete_warning");
            ImmediateDeleteAckPhrase.Text = vm.ImmediateDeleteAckText;
            ImmediateDeleteConfirmInput.Text = "";
            ImmediateDeleteAcknowledgeInput.Text = "";
            ImmediateDeleteDialogError.Visibility = Visibility.Collapsed;
            ImmediateDeleteConfirmButton.IsEnabled = false;
        });
    }

    private void ImmediateDeleteConfirmInput_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_viewModel is null) return;
        _viewModel.ImmediateDeleteConfirmInput = ImmediateDeleteConfirmInput.Text ?? "";
    }

    private void ImmediateDeleteAcknowledgeInput_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_viewModel is null) return;
        _viewModel.ImmediateDeleteAcknowledgeInput = ImmediateDeleteAcknowledgeInput.Text ?? "";
    }

    private void ImmediateDeleteCancel_Click(object sender, RoutedEventArgs e)
    {
        _viewModel?.CancelImmediateDelete();
        ImmediateDeleteDialog.Hide();
    }

    private async void ImmediateDeleteConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.ConfirmImmediateDeleteAsync();
        if (!_viewModel.ImmediateDeleteOpen)
        {
            // Landed — the target row has left the machine's list.
            ImmediateDeleteDialog.Hide();
        }
        else
        {
            // Error (e.g. the nest hard-floor breach) — surface in-dialog (the
            // dialog covers the page ErrorBar) and keep the dialog open.
            ImmediateDeleteDialogError.Text = _viewModel.ErrorMessage ?? "";
            ImmediateDeleteDialogError.Visibility = Visibility.Visible;
        }
    }

    /// <summary><c>snapshot-prune-button</c> — the PREVIEW half (a dry run of the
    /// set's own resting retention policy). The execute lives in the preview.</summary>
    private async void PruneSnapshots_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.PruneCommand.ExecuteAsync(null);
    }

    private async void PruneExecute_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.PruneExecuteAsync();
    }

    private void PruneCancel_Click(object sender, RoutedEventArgs e) =>
        _viewModel?.CancelPrunePreview();

    /// <summary><c>snapshot-check-button</c> — one click RUNS the check; there is no
    /// second confirmation step. A completed check with findings renders in the
    /// result surface, never on <c>error-message</c> (Architectural rule 6).</summary>
    private async void CheckSnapshots_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.CheckIntegrityCommand.ExecuteAsync(null);
    }

    /// <summary>Re-point the folder picker at the machine's list and selection,
    /// REBUILDING THE OPTIONS ONLY WHEN THE OPTION SET ITSELF CHANGED.
    /// <para>Each item's <c>AutomationProperties.Name</c> is the WIRE set name, not
    /// the display label: the shared e2e's <c>driver.select</c> matches a
    /// ComboBoxItem by Name exactly on windows. Same value/label split as
    /// FoldersPage's conflict/paywall pickers.</para>
    /// <para>The whole reconcile runs under the re-entry guard — both clearing the
    /// items and assigning <c>SelectedItem</c> raise SelectionChanged, and letting
    /// either through would dispatch a <c>select_folder</c> for the selection the
    /// machine just handed back: a redundant round trip at best, and a ping-pong
    /// with the machine's disappeared-set fallback at worst (linux's lesson,
    /// inherited via the § Snapshot-list shape ledger).</para>
    /// <para>
    /// The idempotence is load-bearing, not an optimisation. This runs on every
    /// observer tick from two independent paths — <see cref="RefreshSnapshotSurface"/>,
    /// and the <c>SelectedFolder</c> arm of <see cref="ViewModel_PropertyChanged"/>,
    /// which fires from inside <c>Apply()</c> — so an unconditional
    /// <c>Items.Clear()</c> destroyed and recreated every <see cref="ComboBoxItem"/>
    /// FOUR times inside a single <c>SelectionChanged</c> dispatch: measured
    /// 2026-09-09 on Windows, four rebuilds 9 ms apart, re-entrant within the
    /// ComboBox's own event, while the automation client that raised that very
    /// selection still held a provider reference to the item being destroyed.
    /// XAML answered with a stowed <c>E_UNEXPECTED</c> and the process died
    /// <c>0xC000027B</c> — deterministically without
    /// this guard, and it survived only when extra logging happened to shift the
    /// timing.
    /// </para>
    /// <para>
    /// So: reconcile, never rebuild by default. The selection is reconciled
    /// separately from the option set because it changes far more often and
    /// moving it needs no teardown at all — which is also why a folder switch
    /// still repaints correctly with the options left standing.
    /// </para></summary>
    private void PopulateFolderSelector()
    {
        if (_viewModel is null) return;
        _suppressFolderSelection = true;
        try
        {
            if (!FolderOptionsMatchViewModel())
            {
                FolderSelector.Items.Clear();
                foreach (var name in _viewModel.FolderNames)
                {
                    var item = new ComboBoxItem { Content = name, Tag = name };
                    Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, name);
                    FolderSelector.Items.Add(item);
                }
            }
            SelectFolderOption(_viewModel.SelectedFolder);
        }
        finally
        {
            _suppressFolderSelection = false;
        }
    }

    /// <summary>True when the picker already offers exactly the VM's folders, in
    /// order — the predicate that decides whether a teardown is owed at all.</summary>
    private bool FolderOptionsMatchViewModel()
    {
        var want = _viewModel!.FolderNames;
        if (FolderSelector.Items.Count != want.Count) return false;
        for (int i = 0; i < want.Count; i++)
        {
            if (FolderSelector.Items[i] is not ComboBoxItem { Tag: string tag }
                || tag != want[i])
                return false;
        }
        return true;
    }

    /// <summary>Move the picker's selection without touching the option set.
    /// Selecting nothing when the wanted folder is absent matches what a rebuild
    /// used to leave behind (a cleared picker selects nothing), so the visible
    /// end state is unchanged.</summary>
    private void SelectFolderOption(string? wanted)
    {
        foreach (var candidate in FolderSelector.Items)
        {
            if (candidate is ComboBoxItem { Tag: string tag } item && tag == wanted)
            {
                if (!ReferenceEquals(FolderSelector.SelectedItem, item))
                    FolderSelector.SelectedItem = item;
                return;
            }
        }
        if (FolderSelector.SelectedItem is not null) FolderSelector.SelectedItem = null;
    }

    /// Set while PopulateFolderSelector seeds the picker: assigning SelectedItem
    /// raises SelectionChanged, and without this the initial render would re-issue
    /// the select the machine already holds (and stomp a user's in-flight pick).
    private bool _suppressFolderSelection;

    private async void FolderSelector_SelectionChanged(object sender,
        SelectionChangedEventArgs e)
    {
        if (_viewModel is null || _suppressFolderSelection) return;
        if (FolderSelector.SelectedItem is not ComboBoxItem { Tag: string name }) return;
        // The second half of the guard: never dispatch for a selection the machine
        // already holds.
        if (name == _viewModel.SelectedFolder) return;

        await _viewModel.SelectFolderCommand.ExecuteAsync(name);
    }

    /// <summary>Repaint every snapshot-half surface off the VM's projection of the
    /// machine snapshot. Called on mount and on every observer tick; the row and
    /// file lists are ObservableCollections the ListViews already track, so this
    /// covers the scalars they don't.</summary>
    private void RefreshSnapshotSurface()
    {
        if (_viewModel is null) return;
        EmptyText.Visibility = _viewModel.Snapshots.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;
        LastBackedUpText.Text = _viewModel.LastBackedUpText;
        BusyText.Text = _viewModel.BusyText ?? "";
        BusyText.Visibility = _viewModel.BusyText is null ? Visibility.Collapsed : Visibility.Visible;
        CheckResultText.Text = _viewModel.CheckResultText ?? "";
        CheckResultText.Visibility = _viewModel.CheckResultText is null
            ? Visibility.Collapsed : Visibility.Visible;
        CreateProgress.IsActive = _viewModel.IsBusy;
        CreateProgress.Visibility = _viewModel.IsBusy ? Visibility.Visible : Visibility.Collapsed;
        PopulateFolderSelector();
        UpdateActionButtons();
        RenderPrunePreview();
        UpdateFileList();
    }

    /// Single-flight, one predicate: every set-scoped mutating control is armed
    /// only when a set is selected AND no op is in flight. (The per-row buttons
    /// bind the same gate through <c>SnapshotDisplayRow.ActionsEnabled</c>.)
    private void UpdateActionButtons()
    {
        if (_viewModel is null) return;
        var armed = !_viewModel.IsBusy && _viewModel.SelectedFolder is not null;
        BackupNowButton.IsEnabled = armed;
        PruneButton.IsEnabled = armed;
        CheckButton.IsEnabled = armed;
        FolderSelector.IsEnabled = _viewModel.FolderNames.Count > 0 && !_viewModel.IsBusy;
    }

    /// The prune dry-run surface. Execute is offered ONLY from here, and only when
    /// the preview actually names candidates — an armed execute over zero
    /// candidates would promise an effect it cannot have.
    private void RenderPrunePreview()
    {
        if (_viewModel is null) return;
        PrunePreviewLines.Children.Clear();
        foreach (var line in _viewModel.PrunePreviewLines)
        {
            PrunePreviewLines.Children.Add(new TextBlock
            {
                Text = line,
                TextWrapping = TextWrapping.Wrap,
                FontSize = 12,
                Opacity = 0.8,
            });
        }
        PrunePreviewPanel.Visibility = _viewModel.HasPrunePreview
            ? Visibility.Visible : Visibility.Collapsed;
        PruneExecuteButton.Visibility = _viewModel.PruneExecutable
            ? Visibility.Visible : Visibility.Collapsed;
    }

    // ── Backup destinations (management) ────────────────────────────────
    // The page is a thin renderer over BackupsViewModel.Destinations (the VM
    // owns the shared FFI mutate seam; backups.md § Where logic lives). Rows are
    // built in code-behind so each carries the indexed AutomationId + the
    // AutomationProperties.Name that keeps it in the UIA content view (the
    // device-card / conflict-row idiom). The add/edit + remove modals are
    // ContentDialogs whose custom buttons drive the VM and Hide() on success.

    /// <summary>Reconcile the destination rows against the VM's list, REBUILDING
    /// ONLY WHEN THE ID SET (or its order) actually changed. This runs from FIVE
    /// call sites — the <c>Page_Loaded</c> initial hydrate and four gesture
    /// handlers (enroll/edit/remove/reclaim), plus the e2e-only audit-now hook —
    /// so an unconditional <c>Children.Clear()</c> + rebuild tore down every
    /// row's UIA automation peers on EVERY call, even a redundant one landing
    /// right after another with identical data (e.g. <c>Page_Loaded</c>'s slower
    /// initial <c>LoadDestinationsAsync</c> tail completing just after an
    /// enroll's own render) — exactly the class <c>PopulateFolderSelector</c>
    /// was fixed for, here suspected of the same symptom
    /// on a destination row's kind badge. When the
    /// id set is unchanged, existing rows are updated in place (label, usage,
    /// last-upload/-audit, backlog — the live fields; kind cannot change on an
    /// existing row, per <see cref="DestinationEdit_Click"/>'s disabled kind
    /// select) via <see cref="UpdateDestinationRow"/>, never torn down.</summary>
    private void RenderDestinations()
    {
        if (_viewModel is null) return;
        var wanted = _viewModel.Destinations;
        if (DestinationIdsMatch(wanted))
        {
            for (int i = 0; i < wanted.Count; i++)
                UpdateDestinationRow(_destinationRowsById[wanted[i].Id], wanted[i]);
        }
        else
        {
            DestinationsContainer.Children.Clear();
            _destinationRowsById.Clear();
            _destinationRowIds.Clear();
            foreach (var d in wanted)
            {
                var elements = BuildDestinationRow(d);
                DestinationsContainer.Children.Add(elements.Row);
                _destinationRowsById[d.Id] = elements;
                _destinationRowIds.Add(d.Id);
            }
        }
        DestinationsEmpty.Visibility = wanted.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;
        // The standing "every copy you have is on one of your own devices"
        // warning — the shared predicate decides, never a re-derived local count
        // (backups.md § Third destination kind).
        SoleClientDestinationWarning.Text = S.Get("backups/backup_sole_client_destination_warning");
        SoleClientDestinationWarning.Visibility = _viewModel.SoleClientWarningVisible
            ? Visibility.Visible : Visibility.Collapsed;
        RenderOrphanedStoreRow();
        RenderAuditAlerts();
    }

    /// <summary>True when the rendered rows already carry exactly the VM's
    /// destination ids, in order — the predicate that decides whether
    /// <see cref="RenderDestinations"/> owes a teardown at all (mirrors
    /// <c>PopulateFolderSelector.FolderOptionsMatchViewModel</c>).</summary>
    private bool DestinationIdsMatch(IReadOnlyList<BackupDestinationRow> wanted)
    {
        if (wanted.Count != _destinationRowIds.Count) return false;
        for (int i = 0; i < wanted.Count; i++)
            if (wanted[i].Id != _destinationRowIds[i]) return false;
        return true;
    }

    /// <summary>Paint <c>backup-orphaned-store-row</c> from the VM's CACHED
    /// verdict — this reads <see cref="BackupsViewModel.OrphanedStoreText"/> and
    /// never recomputes it (backups.md § Manage backup destinations → *Reclaim
    /// this device's copy*). The verdict costs an agent IPC round trip plus the
    /// agent's own disk walk, so it rides mount / re-map / post-mutation only, and
    /// a render path must never trigger it.</summary>
    private void RenderOrphanedStoreRow()
    {
        if (_viewModel is null) return;
        DestinationReclaimButton.Content = S.Get("backups/backup_destination_reclaim_button");
        OrphanedReseedButton.Content = S.Get("backups/backup_destination_reseed_button");
        OrphanedReseedButton.Visibility = _viewModel.ReseedOnOrphanedStore
            ? Visibility.Visible : Visibility.Collapsed;
        OrphanedReseedButton.IsEnabled = !_viewModel.ReseedRunning;
        if (_viewModel.OrphanedStoreText is { } text)
        {
            OrphanedStoreText.Text = text;
            OrphanedStoreRow.Visibility = Visibility.Visible;
        }
        else
        {
            OrphanedStoreRow.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Rebuild the <c>backup-audit-alert</c> banners from
    /// <see cref="BackupsViewModel.AuditAlerts"/> (backups.md § Audit-alert
    /// surface). Called wherever <see cref="RenderDestinations"/> is, so the
    /// banners stay in lockstep with the status rows they warn about.</summary>
    private void RenderAuditAlerts()
    {
        if (_viewModel is null) return;
        DestinationsAuditAlerts.Children.Clear();
        foreach (var text in _viewModel.AuditAlerts)
        {
            var banner = new TextBlock
            {
                Text = text,
                TextWrapping = TextWrapping.Wrap,
                Foreground = ThemeBrush("SystemFillColorCriticalBrush"),
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(banner, Ids.BackupAuditAlert);
            DestinationsAuditAlerts.Children.Add(banner);
        }
    }

    private static Microsoft.UI.Xaml.Media.Brush? ThemeBrush(string key)
    {
        try
        {
            return Application.Current.Resources.TryGetValue(key, out var v)
                ? v as Microsoft.UI.Xaml.Media.Brush : null;
        }
        catch { return null; }
    }

    /// <summary>The live-updatable pieces of one rendered destination row —
    /// what <see cref="UpdateDestinationRow"/> needs to reconcile in place
    /// without tearing the row down (see <see cref="RenderDestinations"/>).</summary>
    private sealed record DestinationRowElements(
        FrameworkElement Row, TextBlock Label, TextBlock? Usage,
        TextBlock LastUpload, TextBlock LastAudit, TextBlock Backlog, Button EditButton,
        TextBlock UnattestedMark, Button KeepButton, Button ReseedButton);

    private DestinationRowElements BuildDestinationRow(BackupDestinationRow d)
    {
        // AutomationProperties.Name (= the label) keeps the row in the UIA
        // content view so FlaUI's ByAutomationId resolves the child IDs and
        // get_text("backup-destination-status-row") returns the display name.
        var row = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 12,
            Padding = new Thickness(8),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.BackupDestinationStatusRow);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, d.Label);

        var info = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        var label = new TextBlock { Text = d.Label };
        info.Children.Add(label);

        // kind-badge — every row carries one, regardless of kind. Reads the row's
        // OWN discriminator through the shared label, so an unrecognised kind a
        // newer client wrote renders as itself rather than masquerading as a nest
        // (backups.md § Third destination kind → Durability + labeling). Kind
        // cannot change on an existing row (DestinationEdit_Click disables the
        // kind select), so this text is set once and never reconciled.
        var kindBadge = new TextBlock
        {
            Text = BackupsViewModel.KindBadgeText(d.Kind),
            Opacity = 0.6,
            FontSize = 12,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(kindBadge, Ids.BackupDestinationKindBadge);
        info.Children.Add(kindBadge);

        // usage — client-device rows only (ui.yaml). A nest row has no cap and no
        // held-bytes report, so the element is absent rather than empty. Presence
        // is stable per row (tied to Kind, which cannot change), so this is safe
        // to decide once at build time.
        TextBlock? usage = null;
        if (d.Kind == BackupsViewModel.ClientDeviceKind)
        {
            usage = new TextBlock
            {
                Text = _viewModel!.UsageText(d.Id, d.CapacityCapBytes),
                Opacity = 0.6,
                FontSize = 12,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(usage, Ids.BackupDestinationUsage);
            info.Children.Add(usage);
        }

        // last-upload-time / backlog-count render the LIVE per-destination status read
        // (backups.md § Per-destination status read) over the VM's per-row helpers,
        // keyed by destination_id. backlog_count is live; last_upload_time stays "never"
        // until the always-on upload coordinator runs (nest Plan 4 / Track B landed +
        // tier_3-proven, the per-app upload loop is a separate deferred track) —
        // uniform with apple/linux.
        var lastUpload = new TextBlock
        {
            Text = _viewModel!.LastUploadText(d.Id),
            Opacity = 0.6,
            FontSize = 12,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(lastUpload, Ids.BackupDestinationLastUploadTime);
        info.Children.Add(lastUpload);

        // last-audit-time dispatches on the row's kind (backups.md § Audit-alert
        // surface → *The client-device arm*): a client-device custodian carries no
        // address for this client's own audit pass to reach, so it reads its own
        // last-passed self-audit instead ("Self-checked: …"); every other kind keeps
        // this client's independent audit-loop verdict ("Last checked: …").
        var lastAudit = new TextBlock
        {
            Text = _viewModel!.AuditCellText(d.Id),
            Opacity = 0.6,
            FontSize = 12,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(lastAudit, Ids.BackupDestinationLastAuditTime);
        info.Children.Add(lastAudit);

        var backlog = new TextBlock
        {
            Text = _viewModel!.BacklogText(d.Id),
            Opacity = 0.6,
            FontSize = 12,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(backlog, Ids.BackupDestinationBacklogCount);
        info.Children.Add(backlog);

        // The post-succession review pair (succession-aftermath.md § Re-key scope
        // → *Adjudicating what the aftermath carries across*; web and apple are
        // the prior art). Three rules carried with it: ABSENT, not empty, on an
        // ordinary row — Collapsed, which UIA does not count — because after a
        // recovery almost every row is the owner's own and a permanently present
        // mark trains the user past the one that matters; Remove is NOT
        // re-rendered — the row's own backup-destination-remove-button already
        // is it; Keep gets no confirm, being non-destructive and re-decidable.
        // Both are built on every row and toggled by UpdateDestinationRow, since
        // a Keep leaves the id set unchanged and so reconciles in place.
        var unattestedMark = new TextBlock
        {
            Text = S.Get("backups/backup_destination_unattested_mark"),
            TextWrapping = TextWrapping.Wrap,
            FontSize = 12,
            Foreground = ThemeBrush("SystemFillColorCautionBrush"),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(unattestedMark, Ids.BackupDestinationUnattestedMark);
        info.Children.Add(unattestedMark);

        var keep = new Button
        {
            Content = S.Get("backups/backup_destination_keep_button"),
            Tag = d.Id,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(keep, Ids.BackupDestinationKeepButton);
        keep.Click += DestinationKeep_Click;
        info.Children.Add(keep);
        ApplyUnattested(unattestedMark, keep, d.Unattested);
        row.Children.Add(info);

        // backup-destination-reseed-button — this device's own custodian row
        // only, as the shared backup_reseed_rows decides (backups.md § Restore
        // after losing the nest). Built on every row and toggled by
        // ApplyReseed, so an in-place reconcile can move it.
        var reseed = new Button
        {
            Content = S.Get("backups/backup_destination_reseed_button"),
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(reseed, Ids.BackupDestinationReseedButton);
        reseed.Click += DestinationReseed_Click;
        ApplyReseed(reseed, d.Id);
        row.Children.Add(reseed);

        var edit = new Button
        {
            Content = S.Get("backups/backup_destination_edit_button"),
            Tag = d,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(edit, Ids.BackupDestinationEditButton);
        edit.Click += DestinationEdit_Click;
        row.Children.Add(edit);

        var remove = new Button
        {
            Content = S.Get("backups/backup_destination_remove_button"),
            Tag = d.Id,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(remove, Ids.BackupDestinationRemoveButton);
        remove.Click += DestinationRemove_Click;
        row.Children.Add(remove);

        return new DestinationRowElements(
            row, label, usage, lastUpload, lastAudit, backlog, edit, unattestedMark, keep, reseed);
    }

    /// <summary>Paint a row's <c>backup-destination-reseed-button</c> iff the
    /// VM's shared-Rust placement names the row, Collapsed (absent to UIA)
    /// otherwise; disabled while a restore runs.</summary>
    private void ApplyReseed(Button reseed, string destinationId)
    {
        reseed.Visibility = _viewModel!.ReseedOnRow(destinationId)
            ? Visibility.Visible : Visibility.Collapsed;
        reseed.IsEnabled = !_viewModel.ReseedRunning;
    }

    /// <summary>Show the review pair only on a raised row (the shared at-rest
    /// verdict), Collapsed — absent to UIA, not an empty element — otherwise.</summary>
    private static void ApplyUnattested(TextBlock mark, Button keep, bool unattested)
    {
        var visibility = unattested ? Visibility.Visible : Visibility.Collapsed;
        mark.Visibility = visibility;
        keep.Visibility = visibility;
    }

    /// <summary><c>backup-destination-keep-button</c> — clears the row's
    /// post-succession review mark through the shared door; the re-render reads
    /// the VM's re-read list, so the mark goes because the row now says
    /// <c>unattested: false</c>. Errors surface via ErrorMessage → ErrorBar.</summary>
    private async void DestinationKeep_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button { Tag: string id }) return;
        await _viewModel.KeepDestinationAsync(id);
        RenderDestinations();
    }

    /// <summary>Update an existing row's LIVE fields in place — label (an edit
    /// can rename a destination without changing its id), usage, last-upload,
    /// last-audit and backlog — plus the edit button's <c>Tag</c> so a
    /// subsequent edit prefills from the current record. Never touches the
    /// row/child element identities: that is the whole point (see
    /// <see cref="RenderDestinations"/>).</summary>
    private void UpdateDestinationRow(DestinationRowElements elements, BackupDestinationRow d)
    {
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(elements.Row, d.Label);
        elements.Label.Text = d.Label;
        if (elements.Usage is { } usage)
            usage.Text = _viewModel!.UsageText(d.Id, d.CapacityCapBytes);
        elements.LastUpload.Text = _viewModel!.LastUploadText(d.Id);
        elements.LastAudit.Text = _viewModel!.AuditCellText(d.Id);
        elements.Backlog.Text = _viewModel!.BacklogText(d.Id);
        elements.EditButton.Tag = d;
        ApplyUnattested(elements.UnattestedMark, elements.KeepButton, d.Unattested);
        ApplyReseed(elements.ReseedButton, d.Id);
    }

    /// <summary>Build <c>backup-destination-kind-select</c>'s items from the shared
    /// catalog (<c>BackupsViewModel.KindOptions()</c>) — Content = the localized
    /// label (also what the row's kind-badge renders, so they cannot drift), Tag +
    /// AutomationProperties.Name = the WIRE value. Name carries the wire value
    /// (never the label) because the shared e2e's <c>driver.select</c> matches a
    /// ComboBoxItem by Name EXACTLY on windows — the same value/label split as
    /// <see cref="PopulateFolderSelector"/>'s conflict/paywall pickers.
    /// Pure catalog, no state — built once, not repopulated per dialog open.</summary>
    private void PopulateDestinationKindSelect()
    {
        DestinationKindSelect.Items.Clear();
        foreach (var opt in BackupsViewModel.KindOptions())
        {
            var label = S.Resolve(opt.label);
            var item = new ComboBoxItem { Content = label, Tag = opt.value };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, opt.value);
            DestinationKindSelect.Items.Add(item);
        }
    }

    /// The wire discriminator currently selected. No selection (dialog not yet
    /// populated, or a stale reference) falls back to the empty string, which is
    /// simply "not the client-device kind" for <see cref="ApplyDestinationKindSwap"/>'s
    /// purposes — never the client-device sentinel itself.
    private string SelectedDestinationKindWire() =>
        (DestinationKindSelect.SelectedItem as ComboBoxItem)?.Tag as string ?? "";

    /// <summary>Preselect <paramref name="wire"/> in the kind select. An
    /// unrecognised kind (a newer client wrote the row) leaves the selection
    /// alone rather than inventing one — the edit path paints this control
    /// disabled and never reads it back, so the row cannot be rewritten by what
    /// it shows (mirrors linux <c>KindSelect::select_kind</c>).</summary>
    private void SelectDestinationKind(string wire)
    {
        foreach (var obj in DestinationKindSelect.Items)
        {
            if (obj is ComboBoxItem { Tag: string tag } item && tag == wire)
            {
                DestinationKindSelect.SelectedItem = item;
                return;
            }
        }
    }

    /// <summary>Show the fields the selected kind actually has, and hide the ones
    /// it does not — the SWAP is the ratified shape (never a disabled-but-present
    /// URL box): a custodian has no address at all, so an empty URL box would
    /// invite the user to type one nothing could ever use (backups.md § Third
    /// destination kind).</summary>
    private void ApplyDestinationKindSwap()
    {
        var custodian = SelectedDestinationKindWire() == BackupsViewModel.ClientDeviceKind;
        DestinationUrlBox.Visibility = custodian ? Visibility.Collapsed : Visibility.Visible;
        DestinationCapacityBox.Visibility = custodian ? Visibility.Visible : Visibility.Collapsed;
        DestinationCustodianExposureNote.Visibility = custodian ? Visibility.Visible : Visibility.Collapsed;
    }

    private void DestinationKindSelect_SelectionChanged(object sender, SelectionChangedEventArgs e) =>
        ApplyDestinationKindSwap();

    private async void DestinationAdd_Click(object sender, RoutedEventArgs e)
    {
        DestinationDialog.XamlRoot = this.XamlRoot;
        // Prepared only once the gate lets the open through: a refused second
        // open must not wipe the open dialog's fields or its edit target.
        await Controls.Dialogs.ShowAsync(DestinationDialog, prepare: () =>
        {
            _editingDestinationId = null;
            DestinationUrlBox.Text = "";
            DestinationNameBox.Text = "";
            DestinationCapacityBox.Text = "";
            DestinationUrlBox.IsEnabled = true;
            // The kind is editable only while adding, and resets to the catalog
            // default (nest — the kind that actually satisfies "off-site").
            DestinationKindSelect.IsEnabled = true;
            if (DestinationKindSelect.Items.Count > 0) DestinationKindSelect.SelectedIndex = 0;
            ApplyDestinationKindSwap();
            DestinationDialogError.Visibility = Visibility.Collapsed;
            DestinationDialog.Title = S.Get("backups/backup_destination_form_add_title");
        });
    }

    private async void DestinationEdit_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button b || b.Tag is not BackupDestinationRow row) return;
        DestinationDialog.XamlRoot = this.XamlRoot;
        await Controls.Dialogs.ShowAsync(DestinationDialog, prepare: () =>
        {
            _editingDestinationId = row.Id;
            DestinationUrlBox.Text = row.Url;
            DestinationNameBox.Text = row.Label;
            DestinationCapacityBox.Text = row.CapacityCapBytes is { } cap ? ValueFormat.ByteSize(cap) : "";
            DestinationUrlBox.IsEnabled = true;
            // Prefilled from the row so the disabled select paints THIS row's own
            // kind, never the add-dialog default — the kind is not an editable
            // property (re-pointing a live row would keep a destination_id whose
            // registry row and grants describe the other kind).
            SelectDestinationKind(row.Kind);
            DestinationKindSelect.IsEnabled = false;
            ApplyDestinationKindSwap();
            DestinationDialogError.Visibility = Visibility.Collapsed;
            DestinationDialog.Title = S.Get("backups/backup_destination_form_edit_title");
        });
    }

    private void DestinationCancel_Click(object sender, RoutedEventArgs e) => DestinationDialog.Hide();

    private async void DestinationConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        var name = DestinationNameBox.Text ?? "";

        // The kind select is add-only: edit paints it disabled and never reads it
        // back, because re-pointing a live row would keep a destination_id whose
        // registry row and grants describe the other kind.
        if (_editingDestinationId is null && SelectedDestinationKindWire() == BackupsViewModel.ClientDeviceKind)
        {
            // The cap is the kind's only knob, and a blank one is a real choice
            // (uncapped). A non-blank one that cannot be read is a refusal the
            // user sees, never a substituted default — guessing a cap is how a
            // device's disk fills.
            var typed = (DestinationCapacityBox.Text ?? "").Trim();
            ulong? capacityCapBytes = null;
            if (typed.Length > 0)
            {
                capacityCapBytes = BackupsViewModel.TryParseCapacity(typed);
                if (capacityCapBytes is null)
                {
                    DestinationDialogError.Text = S.Get("backups/backup_destination_capacity_invalid");
                    DestinationDialogError.Visibility = Visibility.Visible;
                    return;
                }
            }
            // No interim "Resolving…": there is nothing to resolve — the whole
            // point of the kind is that it has no address.
            var enrolled = await _viewModel.EnrollCustodianAsync(name, capacityCapBytes);
            if (enrolled)
            {
                RenderDestinations();
                DestinationDialog.Hide();
            }
            else
            {
                DestinationDialogError.Text = _viewModel.ErrorMessage ?? "";
                DestinationDialogError.Visibility = Visibility.Visible;
            }
            return;
        }

        var url = (DestinationUrlBox.Text ?? "").Trim();
        if (url.Length == 0) return;

        var ok = _editingDestinationId is null
            ? await _viewModel.AddDestinationAsync(url, name)
            : await _viewModel.EditDestinationAsync(_editingDestinationId, url, name);

        if (ok)
        {
            RenderDestinations();
            DestinationDialog.Hide();
        }
        else
        {
            // The dialog covers the page ErrorBar, so surface the VM's message
            // in-dialog and keep the dialog open so the user can fix the input.
            DestinationDialogError.Text = _viewModel.ErrorMessage ?? "";
            DestinationDialogError.Visibility = Visibility.Visible;
        }
    }

    private async void DestinationRemove_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button b || b.Tag is not string id) return;
        DestinationRemoveDialog.XamlRoot = this.XamlRoot;
        await Controls.Dialogs.ShowAsync(DestinationRemoveDialog, prepare: () =>
        {
            _pendingRemoveId = id;
            // The opt-in reclaim rides ONLY a client-device row's removal — the
            // shared predicate decides, never a kind string compared here. Reset to
            // unchecked on every open: it deletes the owner's only offline copy, so
            // it must never be a sticky choice carried in from a previous dialog.
            DestinationRemoveReclaimCheckBox.IsChecked = false;
            DestinationRemoveReclaimCheckBox.Content =
                S.Get("backups/backup_destination_remove_reclaim_checkbox");
            DestinationRemoveReclaimCheckBox.Visibility =
                _viewModel is not null && _viewModel.RowIsAClientDevice(id)
                    ? Visibility.Visible : Visibility.Collapsed;
        });
    }

    private void DestinationRemoveCancel_Click(object sender, RoutedEventArgs e)
    {
        DestinationRemoveDialog.Hide();
        _pendingRemoveId = null;
    }

    private async void DestinationRemoveConfirm_Click(object sender, RoutedEventArgs e)
    {
        DestinationRemoveDialog.Hide();
        // Read the checkbox BEFORE the await: the dialog is already hidden, and
        // reading UI state across an await is how a re-opened dialog's value
        // leaks into the completing call.
        var alsoReclaim = DestinationRemoveReclaimCheckBox.Visibility == Visibility.Visible
            && DestinationRemoveReclaimCheckBox.IsChecked == true;
        if (_viewModel is not null && _pendingRemoveId is { } id)
            await _viewModel.RemoveDestinationAsync(id, alsoReclaim);
        _pendingRemoveId = null;
        RenderDestinations();
    }

    /// <summary><c>backup-destination-reclaim-button</c> — arms the confirm
    /// modal rather than reaching the agent directly: reclaiming ends this
    /// device's standalone-restore property, which is the reason there is a
    /// modal at all.</summary>
    private async void DestinationReclaim_Click(object sender, RoutedEventArgs e)
    {
        ReclaimConfirmTitle.Text = S.Get("backups/backup_reclaim_confirm_title");
        ReclaimConfirmBody.Text = S.Get("backups/backup_reclaim_confirm_body");
        ReclaimCancelButton.Content = S.Get("backups/backup_reclaim_cancel_button");
        ReclaimConfirmButton.Content = S.Get("backups/backup_reclaim_confirm_button");
        ReclaimConfirmDialog.XamlRoot = this.XamlRoot;
        await Controls.Dialogs.ShowAsync(ReclaimConfirmDialog);
    }

    private void ReclaimCancel_Click(object sender, RoutedEventArgs e) =>
        ReclaimConfirmDialog.Hide();

    /// <summary><c>backup-reclaim-confirm-button</c> — the confirmed standalone
    /// reclaim. The VM surfaces its own failure on <c>error-message</c> and
    /// re-reads the verdict either way, so a refusal leaves the row standing —
    /// the honest state, and the way to retry.</summary>
    private async void ReclaimConfirm_Click(object sender, RoutedEventArgs e)
    {
        ReclaimConfirmDialog.Hide();
        if (_viewModel is not null) await _viewModel.ReclaimOrphanedStoreAsync();
        RenderDestinations();
    }

    /// <summary>Paint <c>backup-destination-reseed-result</c> (present only
    /// after a run) and re-enable or disable every
    /// <c>backup-destination-reseed-button</c> with the running flag.</summary>
    private void RenderReseed()
    {
        if (_viewModel is null) return;
        if (_viewModel.ReseedResultText is { } text)
        {
            ReseedResultText.Text = text;
            ReseedResultText.Visibility = Visibility.Visible;
        }
        else
        {
            ReseedResultText.Visibility = Visibility.Collapsed;
        }
        OrphanedReseedButton.IsEnabled = !_viewModel.ReseedRunning;
        foreach (var elements in _destinationRowsById.Values)
            elements.ReseedButton.IsEnabled = !_viewModel.ReseedRunning;
    }

    /// <summary><c>backup-destination-reseed-button</c> (on the orphaned-store
    /// row or this device's own custodian row) — opens the plain confirm; it
    /// never runs the ceremony itself.</summary>
    private async void DestinationReseed_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || _viewModel.ReseedRunning) return;
        ReseedConfirmTitle.Text = S.Get("backups/backup_reseed_confirm_title");
        ReseedConfirmBody.Text = S.Get("backups/backup_reseed_confirm_body");
        ReseedCancelButton.Content = S.Get("backups/backup_reseed_cancel_button");
        ReseedConfirmButton.Content = S.Get("backups/backup_reseed_confirm_button");
        ReseedConfirmDialog.XamlRoot = this.XamlRoot;
        await Controls.Dialogs.ShowAsync(ReseedConfirmDialog);
    }

    private void ReseedCancel_Click(object sender, RoutedEventArgs e) =>
        ReseedConfirmDialog.Hide();

    /// <summary><c>backup-destination-reseed-confirm-button</c> — the confirmed
    /// restore. The agent's FFI face runs the ceremony and its re-enrollment; the
    /// VM paints how it ended (result view, or a stop on <c>error-message</c>),
    /// and the re-list afterwards moves the rows.</summary>
    private async void ReseedConfirm_Click(object sender, RoutedEventArgs e)
    {
        ReseedConfirmDialog.Hide();
        if (_viewModel is null) return;
        await _viewModel.ReseedAsync();
        RenderDestinations();
    }

    // ── Restore surface ─────────────────────────────────────────────────
    // The page is a thin renderer over the BackupsViewModel restore state (the VM
    // owns the shared FfiSnapshotsClient seam; backups.md § Where logic lives).
    // History rows + divergence items are built in code-behind so each carries the
    // indexed AutomationId + the AutomationProperties.Name that keeps it in the UIA
    // content view (the destination-row / device-card idiom); the banner is a child
    // of its restore-history-item so the scoped e2e query resolves it.

    private void RenderRestoreHistory()
    {
        if (_viewModel is null) return;
        RestoreHistoryContainer.Children.Clear();
        foreach (var row in _viewModel.RestoreHistory)
            RestoreHistoryContainer.Children.Add(BuildRestoreHistoryRow(row));
    }

    private FrameworkElement BuildRestoreHistoryRow(RestoreHistoryRow row)
    {
        var container = new StackPanel { Spacing = 4, Padding = new Thickness(4) };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(container, Ids.RestoreHistoryItem);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(container, row.Description);

        container.Children.Add(new TextBlock { Text = row.Description, TextWrapping = TextWrapping.Wrap });

        // The forensic divergence banner renders only when ≥1 divergence row exists
        // for this snapshot (backups.md § Restore history); clicking opens the modal.
        // A HyperlinkButton reads as a banner/link and is FlaUI-Invokable.
        if (row.HasDivergence)
        {
            var banner = new HyperlinkButton
            {
                Content = row.DivergenceBanner,
                Tag = row,
                Padding = new Thickness(0),
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(banner, Ids.RestoreDivergenceBanner);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(banner, row.DivergenceBanner);
            banner.Click += DivergenceBanner_Click;
            container.Children.Add(banner);
        }

        return container;
    }

    private void DivergenceBanner_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not HyperlinkButton b || b.Tag is not RestoreHistoryRow row) return;
        DivergenceContainer.Children.Clear();
        foreach (var d in row.Divergence)
        {
            var item = new StackPanel { Padding = new Thickness(2) };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(item, Ids.RestoreDivergenceDetailsItem);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, d.Description);
            item.Children.Add(new TextBlock { Text = d.Description, TextWrapping = TextWrapping.Wrap, FontSize = 12 });
            DivergenceContainer.Children.Add(item);
        }
        // Synchronous Visibility flip — the e2e asserts is_visible right after the
        // click with no wait_for (the ContentDialog async open lost that race).
        DivergenceOverlay.Visibility = Visibility.Visible;
    }

    private void DivergenceClose_Click(object sender, RoutedEventArgs e) =>
        DivergenceOverlay.Visibility = Visibility.Collapsed;

    private void RestoreSnapshotSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_viewModel is null) return;
        if (RestoreSnapshotSelect.SelectedItem is RestoreSnapshotOption opt)
            _viewModel.SelectedRestoreSnapshot = opt;
    }

    private void RestoreConfirmInput_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_viewModel is null) return;
        _viewModel.RestoreConfirmInput = RestoreConfirmInput.Text ?? "";
    }

    private async void RestoreConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.RestoreAsync();
        // The restore wrote a restore_history row (the VM reloaded RestoreHistory) —
        // re-render the code-behind-built list so it shows.
        RenderRestoreHistory();
    }

}
