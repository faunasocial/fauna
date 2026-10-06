using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Privacy sub-page (settings.md § Navigation model). Inbox-mode
/// buttons + spam preferences + the email-filter panel. Split out of the former
/// single-scroll <see cref="SettingsPage"/>; constructs its own
/// <see cref="SettingsViewModel"/> from <see cref="ServiceClients"/> in
/// OnNavigatedTo, mirroring the admin sub-pages. Spam-preferences save rides the
/// WS-RPC façade (<c>fauna.spam.set_preferences</c> via <see cref="INestRpcClient"/>);
/// the <c>/api/v1/spam/preferences</c> HTTP twin was deleted nest-side.
/// </summary>
public sealed partial class SettingsPrivacyPage : Page, IAsyncLoadedPage
{
    private SettingsViewModel? _viewModel;
    private INestRpcClient? _rpc;

    /// <summary>The inbox-mode option Buttons, keyed by wire value — built ONCE
    /// (the catalog is static) from <see cref="FaunaFfiMethods.InboxModeOptions"/>
    /// by <see cref="PopulateInboxModeOptions"/>, kept here so
    /// <see cref="ApplyInboxModeSelection"/> can repaint the selected-state visual
    /// on every <see cref="SettingsViewModel.InboxMode"/> change without rebuilding
    /// the rows.</summary>
    private readonly Dictionary<string, Button> _inboxModeButtons = new();

    // ── e2e nav-readiness barrier (IAsyncLoadedPage) ──
    // Completed once BOTH this page's own view-model load AND the hosted
    // EmailFilterControl's independent async load finish — otherwise the windows
    // TestAgent flips ready=true right after the synchronous frame-nav kickoff, while
    // EmailFilterControl.Panel_Loaded's own EmailFiltersListAsync round-trip (and the
    // EditVisibility it computes) is still in flight, and an immediately-following
    // filter_edit_visible()/filter_names() read races a stale/empty list.
    // RunContinuationsAsynchronously so completing it never resumes the awaiting agent
    // inline on the UI thread.
    private TaskCompletionSource _loadComplete =
        new(TaskCreationOptions.RunContinuationsAsynchronously);
    public Task LoadComplete => _loadComplete.Task;

    public SettingsPrivacyPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        // Fresh nav-readiness barrier for THIS navigation (fires before Page_Loaded;
        // robust to a reused/cached page instance). Page_Loaded completes it.
        if (_loadComplete.Task.IsCompleted)
            _loadComplete = new(TaskCreationOptions.RunContinuationsAsynchronously);
        if (e.Parameter is ServiceClients clients)
        {
            _rpc = clients.Rpc!;
            _viewModel = new SettingsViewModel(clients.Nest, clients.Rpc!);
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
            EmailFilterControl.NestClient = clients.Rpc!;
            // Renders immediately with nothing selected (InboxMode starts "" —
            // settings.md item 7's no-default rule) rather than waiting on
            // Page_Loaded's fetch, matching every other page's pre-fetch paint.
            PopulateInboxModeOptions();
        }
    }

    /// <summary>Build the four inbox-mode rows from the shared, static
    /// <see cref="FaunaFfiMethods.InboxModeOptions"/> catalog — called once per
    /// navigation, since the catalog itself never changes mid-session. Each row's
    /// <c>inbox-mode-{value}</c> id, label and description all come from the one
    /// owner (mail-policy-config.md-style rule; settings.md § Where logic lives);
    /// nothing here is a hand-typed literal of the vocabulary.</summary>
    private void PopulateInboxModeOptions()
    {
        InboxModeItems.Children.Clear();
        _inboxModeButtons.Clear();
        foreach (var opt in FaunaFfiMethods.InboxModeOptions())
        {
            var body = new StackPanel { Spacing = 2 };
            body.Children.Add(new TextBlock { Text = Strings.Resolve(opt.@label) });
            body.Children.Add(new TextBlock
            {
                Text = Strings.Resolve(opt.@desc),
                Opacity = 0.7,
                FontSize = 12,
                TextWrapping = TextWrapping.Wrap,
            });
            var button = new Button
            {
                Content = body,
                Tag = opt.@value,
                HorizontalAlignment = HorizontalAlignment.Stretch,
                HorizontalContentAlignment = HorizontalAlignment.Left,
            };
            AutomationProperties.SetAutomationId(button, $"inbox-mode-{opt.@value}");
            button.Click += InboxModeButton_Click;
            InboxModeItems.Children.Add(button);
            _inboxModeButtons[opt.@value] = button;
        }
        ApplyInboxModeSelection();
    }

    /// <summary>Highlight the row matching <see cref="SettingsViewModel.InboxMode"/>
    /// — the richer per-row selected-state apple/android already render (priority
    /// #4: richest pattern wins). While <c>InboxMode</c> is <c>""</c> (unfetched —
    /// settings.md item 7), NO row renders selected; this is the ONLY place that
    /// paints the accent, so the no-default rule cannot be defeated by a stale
    /// visual left over from a prior mode.</summary>
    private void ApplyInboxModeSelection()
    {
        var current = _viewModel?.InboxMode ?? "";
        foreach (var (value, button) in _inboxModeButtons)
            button.Style = value == current ? (Style)Application.Current.Resources["AccentButtonStyle"] : null;
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        try
        {
            if (_viewModel is null) return;
            // Loads inbox mode (and the rest of the account snapshot) from the API,
            // concurrently with EmailFilterControl's own Panel_Loaded-triggered load
            // (already kicked off — a child's Loaded fires before its parent's in
            // WinUI) — overlapped, the page settles in max(vm, panel) rather than
            // their sum.
            await Task.WhenAll(_viewModel.LoadCommand.ExecuteAsync(null), EmailFilterControl.LoadComplete);
        }
        finally
        {
            // The initial load has rendered (success or handled error): release the
            // nav-readiness barrier. ALWAYS in finally so the agent's bounded await
            // never hangs a navigation. See IAsyncLoadedPage.
            _loadComplete.TrySetResult();
        }
    }

    private void ViewModel_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_viewModel is null) return;

        switch (e.PropertyName)
        {
            case nameof(SettingsViewModel.ErrorMessage):
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
            case nameof(SettingsViewModel.InboxMode):
                // The state-protocol mirror (App.TestInboxMode, settings.md item 7):
                // must track the SAME property the page paints from, not just a
                // click's own optimistic value — the VM's own LoadAsync fetch
                // (fauna.inbox.mode.get) also sets InboxMode, and until this case
                // wired it through, a relaunch that re-fetched the real stored mode
                // never updated the mirror, which still reported its "open" seed.
                App.TestInboxMode = _viewModel.InboxMode;
                ApplyInboxModeSelection();
                break;
            case nameof(SettingsViewModel.IsSavingInboxMode):
                InboxModeProgress.IsActive = _viewModel.IsSavingInboxMode;
                InboxModeProgress.Visibility = _viewModel.IsSavingInboxMode ? Visibility.Visible : Visibility.Collapsed;
                break;
        }
    }

    private async void InboxModeButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn || btn.Tag is not string mode) return;
        // No direct App.TestInboxMode write here: ViewModel_PropertyChanged's
        // InboxMode case now mirrors it, and only after SetInboxModeByNameAsync's
        // own InboxMode = mode assignment — which itself waits on
        // _rpc.InboxModeSetAsync(mode) succeeding. A direct write here would
        // report the click's mode before the nest confirmed it (an optimistic
        // update this row's own reference fix rules out).
        await _viewModel.SetInboxModeByNameCommand.ExecuteAsync(mode);
    }

    private async void SaveSpamPrefs_Click(object sender, RoutedEventArgs e)
    {
        if (_rpc is null) return;
        try
        {
            // 0.0–1.0 sliders → per-mille u16 via the shared
            // fauna_protocol::spam::probability_to_per_mille (half-away-from-zero, the
            // single rule every app + the nest share; nest re-clamps to [0,1000]).
            await _rpc.SpamSetPreferencesAsync(
                FaunaFfiMethods.ProbabilityToPerMille(SpamThresholdSlider.Value),
                FaunaFfiMethods.ProbabilityToPerMille(PhishingThresholdSlider.Value));
            // Scroll the save button back into the viewport. The spam-preferences
            // section is taller than the ScrollViewer, so interacting with the
            // threshold sliders (focus-scroll) pushes both the section heading
            // (offscreen-above) and this button (offscreen-below) out of view. WinUI
            // invokes offscreen buttons without scrolling, so without this the
            // post-save `is_visible("save-spam-prefs")` check would fail even though
            // the save succeeded. BringIntoView also surfaces the confirmation to a
            // real user who clicked save from mid-section.
            SaveSpamPrefsButton.StartBringIntoView();
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Core.Services.Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
    }
}
