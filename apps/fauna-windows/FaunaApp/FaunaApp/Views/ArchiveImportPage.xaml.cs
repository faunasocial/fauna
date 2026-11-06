using System;
using System.Threading.Tasks;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Import from other services (docs/goal/behavior/archive-import.md § The
/// wizard and its machine): Source → Archive → Scope → Confirm → Progress → Done over the
/// shared <c>ArchiveImportMachine</c>. A thin view over <see cref="ArchiveImportViewModel"/>
/// — no business logic here (priority #2). Lifts the tui lead page
/// (apps/fauna-tui/src/settings/archive_import.rs); the <c>MailImportPanel</c> shape.
///
/// <para><b>This page spawns the drive loop.</b> After a Start or Resume — or a hydrate
/// that finds a run still <c>Running</c> — it fires <c>DriveImportAsync</c> and a repaint
/// tick, because the machine never spawns its own loop and nothing else would repaint the
/// Progress screen while the run mutates the machine in the background.</para>
///
/// <para><b>The path and date fields are page-local drafts</b>, committed in one ordered
/// dispatch at Open archive / Next, and <see cref="RenderState"/> never writes into
/// them.</para>
/// </summary>
public sealed partial class ArchiveImportPage : Page
{
    /// <summary>The Progress screen's repaint cadence while the drive loop runs — the
    /// mail-import page's tick.</summary>
    private static readonly TimeSpan ProgressTick = TimeSpan.FromMilliseconds(400);

    /// <summary>The page's live view model, for the test agent's anchor commands
    /// (<c>archive_import_pause_after</c> / <c>archive_import_hold_after</c>): null while
    /// no page is mounted or the machine could not be built.</summary>
    internal static ArchiveImportViewModel? CurrentForTest { get; private set; }

    private ServiceClients? _clients;
    private ArchiveImportViewModel? _vm;
    private DispatcherQueueTimer? _progressTimer;
    private bool _importDriving;
    private bool _syncing;
    private bool _cancelArmed;
    private bool _showSkipped;

    public ArchiveImportPage()
    {
        this.InitializeComponent();
    }

    /// <summary><c>x:Bind</c> visibility of an importable category's checkbox.</summary>
    public static Visibility VisibleIf(bool on) => on ? Visibility.Visible : Visibility.Collapsed;

    /// <summary><c>x:Bind</c> visibility of a model-only category's text.</summary>
    public static Visibility VisibleUnless(bool on) => VisibleIf(!on);

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients) _clients = clients;
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        // Both picker vocabularies come from the machine crate's shared fns, filled
        // synchronously before the first await so the pickers are selectable from the
        // moment the page is in the tree.
        if (SourcePicker.ItemsSource is null)
        {
            SourcePicker.ItemsSource = ArchiveImportViewModel.SourceKindLabels();
            AudienceModePicker.ItemsSource = ArchiveImportViewModel.AudienceModeLabels();
            _syncing = true;
            SourcePicker.SelectedIndex = 0;
            AudienceModePicker.SelectedIndex = 0;
            _syncing = false;
        }
        await LoadAsync();
    }

    private void Page_Unloaded(object sender, RoutedEventArgs e)
    {
        if (ReferenceEquals(CurrentForTest, _vm)) CurrentForTest = null;
    }

    private async Task LoadAsync()
    {
        if (_clients?.Rpc is null) return;
        LoadingRing.IsActive = true;
        LoadingRing.Visibility = Visibility.Visible;
        try
        {
            if (_vm is null)
            {
                try
                {
                    var machine = await _clients.Rpc.BuildArchiveImportMachineAsync(
                        _clients.Account.ResolveDeviceId() ?? string.Empty);
                    _vm = new ArchiveImportViewModel(machine);
                }
                catch (Exception ex)
                {
                    ShowUnavailable(S.Error(ex));
                    return;
                }
                CategoriesList.ItemsSource = _vm.Categories;
                CategoryProgressList.ItemsSource = _vm.CategoryProgress;
            }
            CurrentForTest = _vm;
            await _vm.LoadAsync();
            RenderState();
            // A hydrate that found an unfinished import names its archive again: seed the
            // path draft from it once, never over what the user typed.
            if (string.IsNullOrEmpty(ArchivePathInput.Text)) ArchivePathInput.Text = _vm.ArchivePath;
            MaybeDriveImport();
        }
        catch (Exception ex)
        {
            OnError(S.Error(ex));
        }
        finally
        {
            LoadingRing.IsActive = false;
            LoadingRing.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>No machine (the glue refused): paint the fact, hide the wizard.</summary>
    private void ShowUnavailable(string reason)
    {
        Core.Logs.ShellLog.Warn("ArchiveImportPage", $"build machine: {reason}");
        UnavailableText.Visibility = Visibility.Visible;
        SourceGroup.Visibility = Visibility.Collapsed;
        OnError(reason);
    }

    private void OnError(string? message)
    {
        if (string.IsNullOrEmpty(message))
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
        else
        {
            ErrorBar.Message = message;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = message;
        }
    }

    private static Visibility Vis(bool on) => on ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Reflect the VM's scalar state into the UI (the two lists are bound
    /// ObservableCollections). Called after the load, every action and every tick.</summary>
    private void RenderState()
    {
        if (_vm is null) return;
        OnError(_vm.Error);

        var screen = _vm.Screen;
        SourceGroup.Visibility = Vis(screen == ArchiveImportScreen.Source);
        ArchiveGroup.Visibility = Vis(screen == ArchiveImportScreen.Archive);
        ScopeGroup.Visibility = Vis(screen == ArchiveImportScreen.Scope);
        ConfirmGroup.Visibility = Vis(screen == ArchiveImportScreen.Confirm);
        ProgressGroup.Visibility = Vis(screen == ArchiveImportScreen.Progress);
        DoneGroup.Visibility = Vis(screen == ArchiveImportScreen.Done);
        if (screen != ArchiveImportScreen.Done) _showSkipped = false;
        if (screen != ArchiveImportScreen.Progress && _cancelArmed)
        {
            _cancelArmed = false;
            CancelButton.Content = S.Get("archive_import/cancel_button");
        }

        _syncing = true;
        if (SourcePicker.SelectedIndex != _vm.SourceKindIndex) SourcePicker.SelectedIndex = _vm.SourceKindIndex;
        if (AudienceModePicker.SelectedIndex != _vm.AudienceModeIndex) AudienceModePicker.SelectedIndex = _vm.AudienceModeIndex;
        _syncing = false;

        SourceHelp.Text = _vm.SourceHelp;
        SourceExportLink.Content = _vm.SourceExportLinkText;
        ArchiveSummary.Text = _vm.ArchiveSummary;
        ArchiveSummary.Visibility = Vis(!string.IsNullOrEmpty(_vm.ArchiveSummary));
        AudienceSummary.Text = _vm.AudienceSummary;
        HiddenTiersNote.Visibility = Vis(_vm.HiddenTiersUnavailable);

        ConfirmSummary.Text = _vm.ConfirmSummary;
        ProgressSummary.Text = _vm.ProgressSummary;
        ImportProgressBar.Value = _vm.ProgressFraction;
        AutomationProperties.SetHelpText(
            ImportProgressBar, _vm.ProgressFraction.ToString("0.0000", System.Globalization.CultureInfo.InvariantCulture));
        ProgressErrorLog.Text = _vm.ErrorLog;
        PauseButton.IsEnabled = _vm.CanPause;
        ResumeButton.IsEnabled = _vm.CanResume;
        ProgressBackButton.Visibility = Vis(_vm.ProgressConcluded);

        DoneSummary.Text = _vm.DoneSummary;
        DoneErrorLog.Text = _vm.ErrorLog;
        DoneErrorLog.Visibility = Vis(_showSkipped);
        FolderLink.Content = _vm.FolderLinkText;
        ProfilePrefillButton.IsEnabled = _vm.ProfilePrefillAvailable;
    }

    // ── Step 1 — Source ──
    private async void SourcePicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || _vm is null || SourcePicker.SelectedIndex < 0) return;
        await _vm.SelectSourceKindAsync(SourcePicker.SelectedIndex);
        RenderState();
    }

    /// <summary>Hand the export page's address to the OS opener — the shared constant,
    /// never typed by anyone; recorded instead of launched under the harness.</summary>
    private void SourceExportLink_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || string.IsNullOrEmpty(_vm.SourceExportUrl)) return;
        Services.UrlOpener.Open(_vm.SourceExportUrl, "ArchiveImportPage");
    }

    // ── Step 2 — Archive ──

    /// <summary>Open what the path field holds; with an empty field, launch the file
    /// picker, fill the field from the pick and open that.</summary>
    private async void OpenArchive_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        var path = ArchivePathInput.Text;
        if (string.IsNullOrWhiteSpace(path))
        {
            path = await PickArchiveAsync() ?? string.Empty;
            if (path.Length == 0) return;
            ArchivePathInput.Text = path;
        }
        await _vm.OpenArchiveAsync(path);
        RenderState();
    }

    private async Task<string?> PickArchiveAsync()
    {
        var picker = new Windows.Storage.Pickers.FileOpenPicker();
        picker.FileTypeFilter.Add(".zip");
        // WinUI 3 requires initializing the picker with the window handle.
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindow!);
        WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);
        var file = await picker.PickSingleFileAsync();
        return file?.Path;
    }

    // ── Step 3 — Scope ──
    private async void Category_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not CheckBox cb || cb.Tag is not string token || _vm is null) return;
        var selected = cb.IsChecked == true;
        // Synchronously first: the `state` the walk reads must describe the click that
        // just happened (the mail-import mailbox-row reasoning); the re-projection that
        // follows rebuilds the row with the same value.
        AutomationProperties.SetHelpText(cb, selected ? "on" : "off");
        await _vm.ToggleCategoryAsync(token, selected);
        RenderState();
    }

    private async void AudienceModePicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || _vm is null || AudienceModePicker.SelectedIndex < 0) return;
        await _vm.SetAudienceModeAsync(AudienceModePicker.SelectedIndex);
        RenderState();
    }

    private async void ScopeNext_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.ScopeNextAsync(DateFromInput.Text, DateToInput.Text);
        RenderState();
    }

    // ── Navigation + lifecycle ──
    private async void Next_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.NextAsync();
        RenderState();
    }

    private async void Back_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.BackAsync();
        RenderState();
    }

    private async void Start_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.StartAsync();
        RenderState();
        MaybeDriveImport();
    }

    private async void Pause_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.PauseAsync();
        RenderState();
    }

    private async void Resume_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.ResumeAsync();
        RenderState();
        MaybeDriveImport();
    }

    /// <summary>Cancel keeps what landed — arm, then confirm (the mail-import two-click
    /// shape; the tui <c>cancel_armed</c>).</summary>
    private async void Cancel_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        if (!_cancelArmed)
        {
            _cancelArmed = true;
            CancelButton.Content = S.Get("common/confirm_q");
            return;
        }
        _cancelArmed = false;
        CancelButton.Content = S.Get("archive_import/cancel_button");
        await _vm.CancelAsync();
        RenderState();
    }

    // ── Step 6 — Done ──

    /// <summary>The Feed on the imported-posts selection for the archive's platform
    /// (archive-import.md step 6; feed.md § The imported-posts selection): the shared
    /// <c>FeedManager.select_imported</c> on the session's one manager, then the feed.</summary>
    private async void ViewImported_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            if (_vm is not null && _clients?.Rpc is { } rpc)
            {
                var manager = await FaunaApp.Core.FeedManagerHost.GetOrBuildAsync(
                    rpc, () => rpc.BuildFeedManagerAsync(_clients.Account?.ResolveDeviceId()));
                await manager.SelectImported(_vm.ImportedSourceToken);
            }
        }
        catch (System.Exception ex)
        {
            Core.Logs.ShellLog.Warn(nameof(ArchiveImportPage), $"select imported failed: {ex.Message}");
        }
        MainPage.Current?.NavigateToView("feed");
    }

    private void ReviewSkipped_Click(object sender, RoutedEventArgs e)
    {
        _showSkipped = !_showSkipped;
        RenderState();
    }

    private async void ProfilePrefill_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.ApplyProfileAsync();
        RenderState();
    }

    private void FolderLink_Click(object sender, RoutedEventArgs e)
        => MainPage.Current?.NavigateToSettingsSubPage(SettingsNavigation.Folders);

    // ── The drive loop ──

    /// <summary>Spawn the drive loop once, if the VM says the run came back Running, and
    /// repaint on a tick until it ends.</summary>
    private void MaybeDriveImport()
    {
        if (_vm is null || !_vm.ShouldDriveImport || _importDriving) return;
        _importDriving = true;
        StartTick();
        // Fire-and-forget on purpose: the loop's own continuation repaints and reports.
        _ = DriveThenStopAsync();
    }

    private async Task DriveThenStopAsync()
    {
        try
        {
            await _vm!.DriveImportAsync();
        }
        finally
        {
            _importDriving = false;
            _progressTimer?.Stop();
            _progressTimer = null;
            RenderState();
        }
    }

    private void StartTick()
    {
        if (_progressTimer is not null) return;
        _progressTimer = DispatcherQueue.CreateTimer();
        _progressTimer.Interval = ProgressTick;
        _progressTimer.Tick += (_, _) =>
        {
            _vm?.Repaint();
            RenderState();
        };
        _progressTimer.Start();
    }
}
