using System;
using System.Threading.Tasks;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>
/// User-facing "Export mailbox" wizard (docs/goal/behavior/mail-export.md § UX shape):
/// the five-step Format → Scope → Confirm → Progress → Done wizard. A thin view over
/// <see cref="MailExportViewModel"/> (FaunaApp.Core) — which projects the shared
/// <c>fauna_client_mail_settings::MailExportMachine</c> through its UniFFI
/// <c>IMailExportMachine</c> interface (no business logic here, priority #2). Builds the
/// machine over the session's shared, auto-reconnecting WS-RPC connection (the
/// INestRpcClient seam) and hands it to the VM. Hosted by its dedicated Settings shell
/// sub-page <c>SettingsMailExportPage</c>, which supplies <c>ServiceClients</c> via
/// <c>OnNavigatedTo</c> and surfaces this panel's <see cref="ErrorChanged"/> on its own
/// page-level <c>error-message</c>.
/// All five step groups live in one tree; <see cref="RenderState"/> shows only the active
/// step's group. Lifts apps/fauna-linux/src/settings/mail_export.rs.
///
/// <para><b>This panel spawns the export drive loop</b>, exactly as
/// <see cref="MailImportPanel"/> spawns the import's: after a Start/Resume whose
/// post-dispatch snapshot really reports <c>Running</c> it fires
/// <c>DriveExportAsync</c> and starts a 400 ms repaint tick — that loop mutates the
/// machine in the background and nothing else would repaint it. The tick only re-reads
/// the snapshot and re-renders; the Cancel arm is panel state the render never touches,
/// so a tick cannot disarm an armed Cancel (the trap tui's <c>disarm_cancel</c> and
/// linux's widget-state arm each paid for).</para>
/// </summary>
public sealed partial class MailExportPanel : UserControl
{
    /// <summary>How often the Progress screen re-reads the machine while the drive loop
    /// runs — the import twin's tick (<see cref="MailImportPanel"/>), linux's and
    /// FaunaKit's.</summary>
    private static readonly TimeSpan ProgressTick = TimeSpan.FromMilliseconds(400);

    private ServiceClients? _clients;
    private MailExportViewModel? _vm;
    private DispatcherQueueTimer? _progressTimer;

    // Set while RenderState programmatically updates the format picker / scope toggles,
    // so their change handlers don't echo the change back as a dispatch.
    private bool _syncing;
    // True while the Cancel button is armed (first click); the second click dispatches.
    private bool _cancelArmed;

    /// <summary>Raised with the VM's error message (or null to clear) so the host page
    /// surfaces it through its own page-level <c>error-message</c> element.</summary>
    public event Action<string?>? ErrorChanged;

    public MailExportPanel()
    {
        this.InitializeComponent();
    }

    /// <summary>Supplied by the host page (SettingsMailExportPage.OnNavigatedTo) before Loaded fires.</summary>
    internal void Configure(ServiceClients clients) => _clients = clients;

    private async void Panel_Loaded(object sender, RoutedEventArgs e) => await LoadAsync();

    private async Task EnsureVmAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        // Built with key custody over the session's shared, auto-reconnecting WS-RPC
        // connection (the INestRpcClient seam) rather than a per-panel one-shot
        // FfiNestClient.Connect(). The handle is read again at every Start / Resume /
        // Download gesture — it may change with the user after the build.
        var clients = _clients;
        _vm = new MailExportViewModel(
            await clients.Rpc.BuildMailExportMachineAsync(
                clients.Account.Handle ?? string.Empty, MailExportSaveDir.Resolve()),
            () => clients.Account.Handle ?? string.Empty);
        MailboxesList.ItemsSource = _vm.Mailboxes;
        MailboxProgressList.ItemsSource = _vm.MailboxProgress;
    }

    private async Task LoadAsync()
    {
        if (_clients is null) return;
        LoadingRing.IsActive = true;
        LoadingRing.Visibility = Visibility.Visible;
        try
        {
            await EnsureVmAsync();
            await _vm!.LoadCommand.ExecuteAsync(null);
            RenderState();
        }
        catch (Exception ex)
        {
            ErrorChanged?.Invoke(Strings.Error(ex));
        }
        finally
        {
            LoadingRing.IsActive = false;
            LoadingRing.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Reflect the VM's scalar state into the UI (the two lists are bound
    /// ObservableCollections). Called after the load + every action.</summary>
    private void RenderState()
    {
        if (_vm is null) return;

        ErrorChanged?.Invoke(string.IsNullOrEmpty(_vm.Error) ? null : _vm.Error);

        // Step-gated group visibility.
        FormatGroup.Visibility = Vis(_vm.Step == MailExportStep.Format);
        ScopeGroup.Visibility = Vis(_vm.Step == MailExportStep.Scope);
        ConfirmGroup.Visibility = Vis(_vm.Step == MailExportStep.Confirm);
        ProgressGroup.Visibility = Vis(_vm.Step == MailExportStep.Progress);
        DoneGroup.Visibility = Vis(_vm.Step == MailExportStep.Done);

        // Echo-guarded value-via-state controls.
        _syncing = true;
        if (FormatPicker.SelectedIndex != _vm.FormatIndex) FormatPicker.SelectedIndex = _vm.FormatIndex;
        if (StripHeadersToggle.IsOn != _vm.StripHeaders) StripHeadersToggle.IsOn = _vm.StripHeaders;
        _syncing = false;

        MailboxesEmpty.Visibility = Vis(_vm.Mailboxes.Count == 0);

        ConfirmSummary.Text = _vm.ConfirmSummary;
        ProgressSummary.Text = _vm.ProgressSummary;
        ExportProgressBar.Value = _vm.ProgressFraction;
        ErrorLog.Text = _vm.ErrorLog;
        PauseButton.IsEnabled = _vm.CanPause;
        ResumeButton.IsEnabled = _vm.CanResume;
        DoneSummary.Text = _vm.DoneSummary;
        DownloadUrl.Text = _vm.DownloadUrl;
    }

    private static Visibility Vis(bool on) => on ? Visibility.Visible : Visibility.Collapsed;

    // ── Step 1 — Format ──
    private async void FormatPicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || _vm is null || FormatPicker.SelectedIndex < 0) return;
        await _vm.SelectFormatAsync(FormatPicker.SelectedIndex);
        RenderState();
    }

    // ── Step 2 — Scope ──
    private async void Mailbox_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not CheckBox cb || cb.Tag is not string name || _vm is null) return;
        // Synchronously first: the marker the walk reads must describe the click that
        // just happened, not the selection before it. Leaving it to the dispatch's render
        // reports the PREVIOUS state — stale, not merely early. The re-projection that
        // follows rebuilds the row and re-asserts the same value (mirrors mail-import's
        // twin, MailImportPanel.xaml.cs::Mailbox_Click).
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            cb, cb.IsChecked == true ? "on" : "off");
        await _vm.ToggleMailboxAsync(name);
        RenderState();
    }

    private async void DateFrom_LostFocus(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.SetDateFromAsync(DateFromInput.Text.Trim());
    }

    private async void DateTo_LostFocus(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.SetDateToAsync(DateToInput.Text.Trim());
    }

    private async void StripHeaders_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncing || _vm is null || sender is not ToggleSwitch sw) return;
        await _vm.SetStripHeadersAsync(sw.IsOn);
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
        MaybeDriveExport();
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
        MaybeDriveExport();
    }

    /// <summary>Cancel unlinks the partial blob — destructive; arm-then-confirm (mirrors
    /// the linux wire_two_click / MailAliasesPanel).</summary>
    private async void Cancel_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        if (!_cancelArmed)
        {
            _cancelArmed = true;
            var baseLabel = CancelButton.Content;
            CancelButton.Content = "Confirm?";
            await Task.Delay(4000);
            if (_cancelArmed)
            {
                _cancelArmed = false;
                CancelButton.Content = baseLabel;
            }
            return;
        }
        _cancelArmed = false;
        CancelButton.Content = S.Get("mail_export/cancel_button");
        await _vm.CancelAsync();
        RenderState();
    }

    /// <summary>Spawn the export drive loop once, if the VM says the session really came
    /// back Running, and repaint on a tick until it ends.</summary>
    private void MaybeDriveExport()
    {
        if (_vm is null || !_vm.ShouldDriveExport || _progressTimer is not null) return;

        _progressTimer = DispatcherQueue.CreateTimer();
        _progressTimer.Interval = ProgressTick;
        _progressTimer.Tick += (_, _) =>
        {
            _vm.Repaint();
            RenderState();
        };
        _progressTimer.Start();

        // Fire-and-forget on purpose: this is the long-running drive loop, and its own
        // continuation (which repaints and reports) is what "completion" means here.
        _ = DriveThenStopAsync();
    }

    private async Task DriveThenStopAsync()
    {
        try
        {
            await _vm!.DriveExportAsync();
        }
        finally
        {
            _progressTimer?.Stop();
            _progressTimer = null;
            RenderState();
        }
    }

    /// <summary>§ Download flow: fetch, open and save the archive into the user's
    /// Downloads folder. The Done summary then names the saved path — the visible answer
    /// to the press; a refused archive lands on <c>error-message</c> and leaves nothing
    /// behind.</summary>
    private async void Download_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.DownloadAsync();
        RenderState();
    }

    private async void Discard_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.DiscardAsync();
        RenderState();
    }
}
