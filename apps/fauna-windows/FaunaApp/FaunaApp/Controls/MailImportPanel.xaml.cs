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
/// User-facing "Import mailbox" wizard (docs/goal/behavior/mailbox-migration.md § UX
/// shape / § Wizard steps / § Credential handling): the five-step
/// Source → Scope → Confirm → Progress → Done wizard where a person pulls their
/// existing mail off a foreign IMAP server (Gmail / Outlook / iCloud / generic) and
/// into their own nest. A thin view over <see cref="MailImportViewModel"/>, which
/// projects the shared <c>fauna_client_mail_settings::MailImportMachine</c> through its
/// UniFFI <c>IMailImportMachine</c> interface (no business logic here, priority #2).
/// Hosted by <c>SettingsMailImportPage</c>, which supplies <c>ServiceClients</c> via
/// <c>OnNavigatedTo</c> and surfaces this panel's <see cref="ErrorChanged"/> on its own
/// page-level <c>error-message</c>. Lifts apps/fauna-linux/src/settings/mail_import.rs;
/// mirrors this app's <see cref="MailExportPanel"/>.
///
/// <para><b>Three things here are NOT the export twin's shape</b>, each of them
/// load-bearing:</para>
///
/// <para>1. <b>This panel spawns the fetch-drive loop.</b> The shared machine
/// deliberately does not self-spawn <c>run_import</c>, so after a Start/Resume whose
/// post-dispatch snapshot really reports <c>Running</c> the panel fires
/// <c>DriveImportAsync</c> and starts a 400 ms repaint tick — that loop mutates the
/// machine in the background and nothing else would repaint it. Skip either half and the
/// Progress screen sits at zero while the session is genuinely open, which reads exactly
/// like a nest bug.</para>
///
/// <para>2. <b>Two reads the e2e walk makes with NO wait.</b> The bridge acks a command
/// as soon as the handler returns, but a dispatch hops onto the tokio runtime and renders
/// only when it comes back — so both of these answer from local, synchronous truth
/// inside the handler itself, and the async render then re-applies the same thing:
/// picking a provider then typing into the field it reveals
/// (<see cref="ApplySourceVisibility"/>, since a Collapsed field is absent from the UIA
/// tree and the very next keystroke would 404), and toggling a mailbox then reading
/// <em>that</em> row's <c>state</c> (the HelpText flip in
/// <see cref="Mailbox_Click"/>).</para>
///
/// <para>3. <b>The Source/Scope text fields are page-local drafts</b> — these TextBoxes
/// and PasswordBoxes ARE the buffers, committed as ONE ordered multi-action dispatch at
/// Connect/Next through the shared <c>ConnectActions</c>/<c>ScopeNextActions</c>. A
/// per-keystroke dispatch spawns one task per character and lets the Connect click be
/// ordered before the last one, which here means logging into the source with a
/// truncated password. It also means <see cref="RenderState"/> never writes back into a
/// text field, so the caret never jumps and a failed-connect retry keeps the credentials
/// on screen ("the user can retry from this screen without re-entering credentials",
/// § Wizard steps step 2).</para>
/// </summary>
public sealed partial class MailImportPanel : UserControl
{
    /// <summary>How often the Progress screen re-reads the machine while the drive loop
    /// runs. Matches the linux lead's tick and android's ticker coroutine.</summary>
    private static readonly TimeSpan ProgressTick = TimeSpan.FromMilliseconds(400);

    private ServiceClients? _clients;
    private MailImportViewModel? _vm;
    private DispatcherQueueTimer? _progressTimer;

    // Set while RenderState programmatically updates the two pickers, so their change
    // handlers don't echo the change back as a dispatch.
    private bool _syncing;
    // True while the Cancel button is armed (first click); the second click dispatches.
    private bool _cancelArmed;

    /// <summary>Raised with the VM's error message (or null to clear) so the host page
    /// surfaces it through its own page-level <c>error-message</c> element.</summary>
    public event Action<string?>? ErrorChanged;

    public MailImportPanel()
    {
        this.InitializeComponent();
    }

    /// <summary>Supplied by the host page (SettingsMailImportPage.OnNavigatedTo) before Loaded fires.</summary>
    internal void Configure(ServiceClients clients) => _clients = clients;

    private async void Panel_Loaded(object sender, RoutedEventArgs e)
    {
        // Both picker vocabularies come from shared Rust (importSourceKindLabel /
        // importTlsModeLabel — the exportFormatLabel precedent). Filled synchronously
        // BEFORE the first await so the pickers are populated and selectable from the
        // moment the panel is in the tree, not one round trip later.
        if (SourcePicker.ItemsSource is null)
        {
            SourcePicker.ItemsSource = MailImportViewModel.SourceKindLabels();
            TlsModePicker.ItemsSource = MailImportViewModel.TlsModeLabels();
            _syncing = true;
            SourcePicker.SelectedIndex = 0;
            TlsModePicker.SelectedIndex = 0;
            _syncing = false;
            ApplySourceVisibility(0);
        }
        await LoadAsync();
    }

    private async Task EnsureVmAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        // User-class machine — built over the session's shared, auto-reconnecting WS-RPC
        // connection (the INestRpcClient seam), the same as every machine beside it.
        _vm = new MailImportViewModel(await _clients.Rpc.BuildMailImportMachineAsync());
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
            // Hydrate can land straight on the Progress screen of a session that was
            // already running when the app closed; that session needs the drive loop
            // just as much as a freshly started one does.
            MaybeDriveImport();
        }
        catch (Exception ex)
        {
            ErrorChanged?.Invoke(S.Error(ex));
        }
        finally
        {
            LoadingRing.IsActive = false;
            LoadingRing.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Reflect the VM's scalar state into the UI (the two lists are bound
    /// ObservableCollections). Called after the load and every action.</summary>
    private void RenderState()
    {
        if (_vm is null) return;

        ErrorChanged?.Invoke(string.IsNullOrEmpty(_vm.Error) ? null : _vm.Error);

        // Step-gated group visibility. A Collapsed group is absent from the UIA tree, so
        // this is also what keeps the shared wizard-back/next ids unique.
        SourceGroup.Visibility = Vis(_vm.Step == MailImportStep.Source);
        ScopeGroup.Visibility = Vis(_vm.Step == MailImportStep.Scope);
        ConfirmGroup.Visibility = Vis(_vm.Step == MailImportStep.Confirm);
        ProgressGroup.Visibility = Vis(_vm.Step == MailImportStep.Progress);
        DoneGroup.Visibility = Vis(_vm.Step == MailImportStep.Done);

        // Echo-guarded value-via-state controls. The text fields are deliberately NOT
        // written back — see the class docs' point 3.
        _syncing = true;
        if (SourcePicker.SelectedIndex != _vm.SourceKindIndex) SourcePicker.SelectedIndex = _vm.SourceKindIndex;
        if (TlsModePicker.SelectedIndex != _vm.TlsModeIndex) TlsModePicker.SelectedIndex = _vm.TlsModeIndex;
        _syncing = false;
        ApplySourceVisibility(_vm.SourceKindIndex);

        MailboxesEmpty.Visibility = Vis(_vm.Mailboxes.Count == 0);

        ConfirmSummary.Text = _vm.ConfirmSummary;
        ProgressSummary.Text = _vm.ProgressSummary;
        ImportProgressBar.Value = _vm.ProgressFraction;
        ErrorLog.Text = _vm.ErrorLog;
        PauseButton.IsEnabled = _vm.CanPause;
        ResumeButton.IsEnabled = _vm.CanResume;
        DoneSummary.Text = _vm.DoneSummary;
    }

    /// <summary>The per-provider Source-field table (§ Wizard steps step 1's "Required
    /// fields"), applied as a pure function of the pick. Runs inside the picker's own
    /// SelectionChanged handler, before any await: the walk picks a provider and types
    /// into the field it reveals with nothing in between.</summary>
    private void ApplySourceVisibility(int sourceKindIndex)
    {
        var fields = MailImportViewModel.FieldsFor(sourceKindIndex);
        AppPasswordInput.Visibility = Vis(fields.ShowAppPassword);
        AppPasswordHelp.Visibility = Vis(fields.ShowAppPassword);
        AppPasswordHelp.Text = fields.ShowAppPassword
            ? S.Get(sourceKindIndex == 2
                ? "mail_import/source_app_password_help_icloud"
                : "mail_import/source_app_password_help_gmail")
            : string.Empty;
        OauthButton.Visibility = Vis(fields.ShowOauthButton);
        HostInput.Visibility = Vis(fields.ShowImapFields);
        PortInput.Visibility = Vis(fields.ShowImapFields);
        TlsModePicker.Visibility = Vis(fields.ShowImapFields);
        UsernameInput.Visibility = Vis(fields.ShowImapFields);
        PasswordInput.Visibility = Vis(fields.ShowImapFields);
    }

    private static Visibility Vis(bool on) => on ? Visibility.Visible : Visibility.Collapsed;

    // ── Step 1 — Source ──
    private async void SourcePicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || SourcePicker.SelectedIndex < 0) return;
        // Synchronously first — see the class docs' point 2.
        ApplySourceVisibility(SourcePicker.SelectedIndex);
        if (_vm is null) return;
        await _vm.SelectSourceKindAsync(SourcePicker.SelectedIndex);
        RenderState();
        // Re-seed the draft TextBoxes from the snapshot THIS dispatch just wrote —
        // never inside RenderState() itself, which runs after every action (a rejected
        // Connect included) and must not clobber what the user typed —
        // apple's syncDraftsFromSnapshot shape. Unconditional: Generic has no preset
        // of its own, so it keeps whatever the previous kind painted, matching every
        // other app.
        HostInput.Text = _vm.Host;
        PortInput.Text = _vm.Port;
    }

    private async void TlsModePicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || _vm is null || TlsModePicker.SelectedIndex < 0) return;
        await _vm.SetTlsModeAsync(TlsModePicker.SelectedIndex);
        RenderState();
    }

    /// <summary>Step 1→3. The whole Source form commits at once; which password box holds
    /// the secret depends on the picked provider, exactly as the field table shows it.</summary>
    private async void Connect_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        var fields = MailImportViewModel.FieldsFor(SourcePicker.SelectedIndex);
        var password = fields.ShowAppPassword ? AppPasswordInput.Password : PasswordInput.Password;
        await _vm.ConnectAsync(HostInput.Text, PortInput.Text, UsernameInput.Text, password);
        RenderState();
    }

    // ── Step 2 — Scope ──
    private async void Mailbox_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not CheckBox cb || cb.Tag is not string name || _vm is null) return;
        // Synchronously first: the marker the walk reads must describe the click that
        // just happened, not the selection before it. Leaving it to the dispatch's render
        // reports the PREVIOUS state — stale, not merely early. The re-projection that
        // follows rebuilds the row and re-asserts the same value.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            cb, cb.IsChecked == true ? "on" : "off");
        await _vm.ToggleMailboxAsync(name);
        RenderState();
    }

    private async void ScopeNext_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.ScopeNextAsync(DateFromInput.Text, MaxSizeInput.Text);
        RenderState();
    }

    // ── Navigation + lifecycle ──
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

    /// <summary>Cancel stops a durable session — arm-then-confirm, mirroring the export
    /// twin and the linux <c>wire_two_click</c>. Already-imported messages are kept
    /// (§ UX shape step 5).</summary>
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
        CancelButton.Content = S.Get("mail_import/cancel_button");
        await _vm.CancelAsync();
        RenderState();
    }

    /// <summary>Spawn the fetch-drive loop once, if the VM says the session really came
    /// back Running, and repaint on a tick until it ends.</summary>
    private void MaybeDriveImport()
    {
        if (_vm is null || !_vm.ShouldDriveImport || _progressTimer is not null) return;

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
        // NOT ConfigureAwait(false) inside the VM — a WinUI VM that configures the
        // context away throws a silent COMException on the bound collections.
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
            _progressTimer?.Stop();
            _progressTimer = null;
            RenderState();
        }
    }

    // ── Step 5 — Done ──
    // Neither deep-link is backed by a MailImportAction — the shared machine never wired
    // a "view inbox" / "skip log" RPC — so both navigate to Conversations, where mail
    // lives. The tui lead app's own resolution: a real navigation, not a stub. "Review
    // skipped" cannot deep-link to a skip-log page that exists nowhere in the app yet;
    // the error log lives inline on the Progress screen instead.
    private void ViewImported_Click(object sender, RoutedEventArgs e)
        => Views.MainPage.Current?.NavigateToView("conversations");

    private void ReviewSkipped_Click(object sender, RoutedEventArgs e)
        => Views.MainPage.Current?.NavigateToView("conversations");
}
