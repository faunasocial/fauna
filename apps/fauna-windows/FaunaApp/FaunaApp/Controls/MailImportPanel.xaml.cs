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
/// text field on a repaint — only on the edge a step or prompt opens, into a field the
/// user has not typed in (<see cref="SeedScopeDraftOnEntry"/>,
/// <see cref="RenderResumeCredentials"/>) — so the caret never jumps and a failed-connect retry keeps the credentials
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
    // Review skipped's reveal of the skip log on the Done screen; cleared off Done.
    private bool _showSkipped;

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
            ResumeTlsModePicker.ItemsSource = MailImportViewModel.TlsModeLabels();
            _syncing = true;
            SourcePicker.SelectedIndex = 0;
            TlsModePicker.SelectedIndex = 0;
            ResumeTlsModePicker.SelectedIndex = 0;
            _syncing = false;
            ApplySourceVisibility(0);
        }
        Subscribe();
        await LoadAsync();
    }

    private async Task EnsureVmAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        // User-class machine — built over the session's shared, auto-reconnecting WS-RPC
        // connection (the INestRpcClient seam), the same as every machine beside it, and
        // HELD for the app session so a finished import's Done and skip log survive
        // leaving the page (§ Wizard steps step 6). The VM and the drafts stay per visit.
        _vm = new MailImportViewModel(await MailImportMachineHolder.GetAsync(_clients.Rpc));
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
            PrefillSourceDraftsOnce();
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
        if (_vm.Step != MailImportStep.Done) _showSkipped = false;
        DoneErrorLogGroup.Visibility = Vis(_showSkipped);
        DoneErrorLog.Text = _vm.ErrorLog;

        // Echo-guarded value-via-state controls. The text fields are deliberately NOT
        // written back — see the class docs' point 3.
        _syncing = true;
        if (SourcePicker.SelectedIndex != _vm.SourceKindIndex) SourcePicker.SelectedIndex = _vm.SourceKindIndex;
        if (TlsModePicker.SelectedIndex != _vm.TlsModeIndex) TlsModePicker.SelectedIndex = _vm.TlsModeIndex;
        if (ResumeTlsModePicker.SelectedIndex != _vm.TlsModeIndex) ResumeTlsModePicker.SelectedIndex = _vm.TlsModeIndex;
        _syncing = false;
        ApplySourceVisibility(_vm.SourceKindIndex);
        SeedScopeDraftOnEntry();

        MailboxesEmpty.Visibility = Vis(_vm.Mailboxes.Count == 0);

        ConfirmSummary.Text = _vm.ConfirmSummary;
        ProgressSummary.Text = _vm.ProgressSummary;
        ImportProgressBar.Value = _vm.ProgressFraction;
        // The bar's fraction (quota_fraction, four decimals) as a bare HelpText value —
        // one attribute is not a pack, so the flaui bridge returns it verbatim.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            ImportProgressBar,
            _vm.ProgressFraction.ToString("0.0000", System.Globalization.CultureInfo.InvariantCulture));
        RenderResumeCredentials();
        QuotaGroup.Visibility = Vis(_vm.MailboxFull);
        ErrorLog.Text = _vm.ErrorLog;
        PauseButton.IsEnabled = _vm.CanPause;
        ResumeButton.IsEnabled = _vm.CanResume;
        // A failed import offers Dismiss alone: Pause, Resume, Cancel and the restart
        // offer are not painted (the machine refuses each on a terminal session). The
        // restart offer is otherwise painted only while a rebuilt mailbox is listed.
        PauseButton.Visibility = Vis(!_vm.IsFailed);
        ResumeButton.Visibility = Vis(!_vm.IsFailed);
        CancelButton.Visibility = Vis(!_vm.IsFailed);
        RestartButton.Visibility = Vis(_vm.HasRebuiltMailboxes && !_vm.IsFailed);
        DismissButton.Visibility = Vis(_vm.IsFailed);
        DoneSummary.Text = _vm.DoneSummary;

        ForgetSourceButton.Visibility = Vis(_vm.SourceRemembered);
        RenderMicrosoftRows();
        // The toggle reflects the snapshot, never a click echo.
        ImportDuplicatesToggle.IsChecked = _vm.ImportDuplicates;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            ImportDuplicatesToggle, _vm.ImportDuplicates ? "on" : "off");
    }

    // The last-rendered Scope-step state, so the max-size draft is seeded once when the
    // step opens.
    private bool _scopeShown;

    /// <summary>The Scope step's max-size draft starts at the machine's own value — its
    /// 50 MiB default, or the size a previous Next committed — the user can lower it
    /// (§ Wizard steps step 3). Written only on the edge into the Scope step and only
    /// while the field is empty, so a value the user typed is never overwritten and an
    /// action on the step (a mailbox toggle) never refills a field the user is editing:
    /// the apple <c>syncDraftsFromSnapshot</c> shape, the edge form of
    /// <see cref="RenderResumeCredentials"/>'s port seed.</summary>
    private void SeedScopeDraftOnEntry()
    {
        if (_vm is null) return;
        var on = _vm.Step == MailImportStep.Scope;
        if (on && !_scopeShown && string.IsNullOrEmpty(MaxSizeInput.Text))
        {
            MaxSizeInput.Text = _vm.MaxSizeMb;
        }
        _scopeShown = on;
    }

    // The last-rendered resume-prompt state, so the port draft is seeded once when the
    // prompt turns on and the password drafts are cleared when it turns off.
    private bool _resumePromptShown;

    /// <summary>The restart-resume credential prompt: the session provider's own field
    /// beside Resume while <c>resumeNeedsCredentials</c> is set (§ Resume protocol step 2).
    /// The drafts are the user's buffers, so they are written only on the on/off edge.</summary>
    private void RenderResumeCredentials()
    {
        if (_vm is null) return;
        var on = _vm.ResumeNeedsCredentials;
        ResumeCredentialGroup.Visibility = Vis(on);
        if (on && !_resumePromptShown)
        {
            ResumePortInput.Text = _vm.ResumePort;
        }
        else if (!on && _resumePromptShown)
        {
            ResumePasswordInput.Password = string.Empty;
            ResumeAppPasswordInput.Password = string.Empty;
        }
        _resumePromptShown = on;
        if (!on) return;

        var fields = MailImportViewModel.FieldsFor(_vm.ResumeKindIndex);
        var outlook = fields.ShowOauthButton;
        ResumeNeedsText.Text = S.Get(outlook
            ? "mail_import/resume_needs_microsoft"
            : "mail_import/resume_needs_password");
        ResumeAppPasswordInput.Visibility = Vis(fields.ShowAppPassword);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            ResumeAppPasswordInput, AppPasswordHelpText(_vm.ResumeKindIndex));
        ResumePortInput.Visibility = Vis(fields.ShowImapFields);
        ResumeTlsModePicker.Visibility = Vis(fields.ShowImapFields);
        ResumePasswordInput.Visibility = Vis(fields.ShowImapFields);
    }

    private static string AppPasswordHelpText(int sourceKindIndex) => S.Get(sourceKindIndex == 2
        ? "mail_import/source_app_password_help_icloud"
        : "mail_import/source_app_password_help_gmail");

    /// <summary>Fill the Source drafts from a remembered source's prefilled snapshot, once
    /// per visit and never over what the user typed (the drafts are the user's buffers,
    /// so <see cref="RenderState"/> never writes them).</summary>
    private void PrefillSourceDraftsOnce()
    {
        if (_vm is null || _draftsPrefilled) return;
        _draftsPrefilled = true;
        if (!_vm.SourceRemembered) return;
        if (!string.IsNullOrEmpty(HostInput.Text) || !string.IsNullOrEmpty(UsernameInput.Text)) return;
        HostInput.Text = _vm.Host;
        PortInput.Text = _vm.Port;
        UsernameInput.Text = _vm.Username;
    }

    private bool _draftsPrefilled;
    // Which background loops the shared repaint tick is serving.
    // The import drive lives on the held machine, not the panel: see
    // MailImportMachineHolder.ImportDriving.
    private static bool _importDriving
    {
        get => MailImportMachineHolder.ImportDriving;
        set => MailImportMachineHolder.ImportDriving = value;
    }
    private bool _signInDriving;
    private INestRpcClient? _subscribed;

    /// <summary>Follow the account's import events: an open wizard re-reads as a
    /// repaint (a page not open does nothing). Subscribed only while loaded.</summary>
    private void Subscribe()
    {
        var rpc = _clients?.Rpc;
        if (rpc is null || ReferenceEquals(_subscribed, rpc)) return;
        Unsubscribe();
        rpc.MailImportPushChanged += OnImportStale;
        _subscribed = rpc;
    }

    private void Unsubscribe()
    {
        if (_subscribed is null) return;
        _subscribed.MailImportPushChanged -= OnImportStale;
        _subscribed = null;
    }

    private void Panel_Unloaded(object sender, RoutedEventArgs e)
    {
        Unsubscribe();
        // The tick repaints this panel only; the drive loop (if any) keeps running on the
        // held machine and the next visit's panel starts its own tick.
        _progressTimer?.Stop();
        _progressTimer = null;
    }

    /// <summary>A repaint, not a gesture: <c>_cancelArmed</c> and the Cancel label are
    /// left alone, so a push never disarms a confirm the user is looking at (the tui's
    /// <c>Op::ResyncMailImport</c> shape). Skipped until the first load built the VM.</summary>
    private async void OnImportStale()
    {
        if (_vm is null) return;
        await _vm.ResyncAsync();
        RenderState();
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
        AppPasswordHelp.Text = fields.ShowAppPassword ? AppPasswordHelpText(sourceKindIndex) : string.Empty;
        // The same sentence is the field's `help` attribute (a bare HelpText value) —
        // what the cross-app journey reads to check the provider's own guidance.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            AppPasswordInput, AppPasswordHelp.Text);
        OauthButton.Visibility = Vis(fields.ShowOauthButton);
        HostInput.Visibility = Vis(fields.ShowImapFields);
        PortInput.Visibility = Vis(fields.ShowImapFields);
        TlsModePicker.Visibility = Vis(fields.ShowImapFields);
        // The one Source field painted for every provider (the tui lead): a Gmail or
        // iCloud app password still logs in as an address, and the remembered source's
        // username must stay readable after a Forget resets the provider.
        UsernameInput.Visibility = Visibility.Visible;
        // Outlook asks for the Microsoft account's address, the XOAUTH2 user.
        UsernameInput.PlaceholderText = S.Get(fields.ShowOauthButton
            ? "mail_import/source_outlook_address_label"
            : "mail_import/source_username_placeholder");
        PasswordInput.Visibility = Vis(fields.ShowImapFields);
        // The Microsoft grant connects by itself — no Connect button beside it.
        ConnectButton.Visibility = Vis(fields.ShowConnectButton);
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
        // The prompt's drafts ride the shared ResumeActions sequence; which password box
        // holds the secret depends on the session's provider, as on the Source step.
        var fields = MailImportViewModel.FieldsFor(_vm.ResumeKindIndex);
        var password = fields.ShowAppPassword ? ResumeAppPasswordInput.Password : ResumePasswordInput.Password;
        await _vm.ResumeAsync(ResumePortInput.Text, password);
        RenderState();
        MaybeDriveImport();
    }

    /// <summary>Full-mailbox notice's Skip the rest; the run is spawned when the shared
    /// <c>ImportActionStartsRun</c> says so.</summary>
    private async void SkipRest_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.SkipRestAsync();
        RenderState();
        MaybeDriveImport();
    }

    /// <summary>The resume prompt's connection-security picker dispatches SetTlsMode itself,
    /// as on the Source step.</summary>
    private async void ResumeTlsModePicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing || _vm is null || ResumeTlsModePicker.SelectedIndex < 0) return;
        await _vm.SetTlsModeAsync(ResumeTlsModePicker.SelectedIndex);
        RenderState();
    }

    /// <summary>Drop the rebuilt mailboxes' cursors and resume; the run is spawned
    /// when the shared <c>ImportActionStartsRun</c> says the dispatch obliges it.</summary>
    private async void Restart_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.RestartRebuiltMailboxesAsync();
        RenderState();
        MaybeDriveImport();
    }

    /// <summary>Leave a failed import: back to Source with the session discarded.</summary>
    private async void Dismiss_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _cancelArmed = false;
        await _vm.DismissAsync();
        RenderState();
    }

    /// <summary>Delete the remembered source and empty the Source drafts.</summary>
    private async void ForgetSource_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.ForgetSourceAsync();
        HostInput.Text = string.Empty;
        PortInput.Text = string.Empty;
        UsernameInput.Text = string.Empty;
        AppPasswordInput.Password = string.Empty;
        PasswordInput.Password = string.Empty;
        RenderState();
    }

    private async void ImportDuplicates_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        // The dispatch is the negation of the snapshot value, never the click's own state.
        await _vm.SetImportDuplicatesAsync(!_vm.ImportDuplicates);
        RenderState();
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
            CancelButton.Content = S.Get("common/confirm_q");
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
        if (_vm is null || !_vm.ShouldDriveImport) return;
        if (_importDriving)
        {
            // A previous visit's loop is still running on the held machine: follow it
            // with this panel's tick instead of spawning a second run_import.
            StartTick();
            return;
        }

        _importDriving = true;
        StartTick();

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
            _importDriving = false;
            StopTickIfIdle();
            RenderState();
        }
    }

    /// <summary>Spawn the Microsoft sign-in's polling half once, if the VM says
    /// Microsoft's code is out. The same two-task shape as the import drive: the poll
    /// suspends until the grant ends (it connects by itself and the wizard moves on to
    /// Scope), so the tick that paints the code and the step change runs beside it.</summary>
    private void MaybeDriveMicrosoftSignIn()
    {
        if (_vm is null || !_vm.ShouldDriveMicrosoftSignIn || _signInDriving) return;

        _signInDriving = true;
        StartTick();
        _ = SignInThenStopAsync();
    }

    private async Task SignInThenStopAsync()
    {
        try
        {
            await _vm!.DriveMicrosoftSignInAsync();
        }
        finally
        {
            _signInDriving = false;
            StopTickIfIdle();
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
            StopTickIfIdle();
        };
        _progressTimer.Start();
    }

    private void StopTickIfIdle()
    {
        if (_importDriving || _signInDriving) return;
        _progressTimer?.Stop();
        _progressTimer = null;
    }

    // ── Outlook — Sign in with Microsoft ──
    private async void Oauth_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.SignInWithMicrosoftAsync(UsernameInput.Text);
        RenderState();
        MaybeDriveMicrosoftSignIn();
    }

    /// <summary>Hand the sign-in address to the OS opener — purely local, and recorded
    /// instead of launched under the e2e harness (<c>UrlOpener</c>).</summary>
    private void OauthOpenBrowser_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || string.IsNullOrEmpty(_vm.MicrosoftUrl)) return;
        Services.UrlOpener.Open(_vm.MicrosoftUrl, "MailImportPanel");
    }

    /// <summary>Paint the Outlook sign-in rows from the VM: the button's state, the code
    /// row only while Microsoft's code is out, and the sentence under them.</summary>
    private void RenderMicrosoftRows()
    {
        if (_vm is null) return;
        var available = _vm.MicrosoftSignInAvailable;
        var outlook = MailImportViewModel.FieldsFor(_vm.SourceKindIndex).ShowOauthButton;
        OauthButton.IsEnabled = available && !_vm.MicrosoftSignInInFlight;
        var waiting = outlook && _vm.MicrosoftCode.Length > 0;
        OauthCodeRow.Visibility = Vis(waiting);
        if (waiting)
        {
            OauthCode.Text = S.Format("mail_import/microsoft_code_fmt", _vm.MicrosoftUrl, _vm.MicrosoftCode);
            // The code and the address each ride alone as attributes — the observables the
            // cross-app journey reads (the flaui bridge unpacks key=value;key=value).
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
                OauthCode, $"code={_vm.MicrosoftCode};url={_vm.MicrosoftUrl}");
        }
        var note = outlook && !available ? S.Get("mail_import/microsoft_unavailable")
            : waiting ? S.Get("mail_import/microsoft_waiting")
            : null;
        OauthNote.Visibility = Vis(note is not null);
        if (note is not null) OauthNote.Text = note;
    }

    // ── Step 5 — Done ──
    // View imported is not backed by a MailImportAction — the shared machine never wired a
    // "view inbox" RPC — so it navigates to Conversations, where mail lives.
    private void ViewImported_Click(object sender, RoutedEventArgs e)
        => Views.MainPage.Current?.NavigateToView("conversations");

    // Review skipped toggles the skip log on the Done screen (the snapshot's error log,
    // which the shared machine keeps through Done); page-local, like the archive import.
    private void ReviewSkipped_Click(object sender, RoutedEventArgs e)
    {
        _showSkipped = !_showSkipped;
        RenderState();
    }
}
