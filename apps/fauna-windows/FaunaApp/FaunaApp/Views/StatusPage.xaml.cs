using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// Displays service status: connection state, version, uptime, and sync summary.
/// Queries all three services (nest, sync, bridge).
/// </summary>
public sealed partial class StatusPage : Page
{
    private StatusViewModel? _viewModel;

    public StatusPage()
    {
        this.InitializeComponent();
        // A relay reply folded while this page is up repaints the region section in
        // place (RegionPlaneHost raises off the UI thread — marshal back).
        Loaded += (_, _) => RegionPlaneHost.Changed += OnRegionChanged;
        Unloaded += (_, _) => RegionPlaneHost.Changed -= OnRegionChanged;
    }

    private void OnRegionChanged() => DispatcherQueue.TryEnqueue(PaintRegion);

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _viewModel = new StatusViewModel(
                clients.Nest,
                clients.Rpc!,
                new AgentStatusProbe(() => App.CurrentSyncAgent?.Channel));
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
            NestUrlLabel.Text = S.Get("settings/nest_url");
            NestUrlText.Text = clients.Account.NestUrl ?? "--";
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        PaintRegion();
        await LoadStatusAsync();
    }

    private async void RefreshButton_Click(object sender, RoutedEventArgs e)
    {
        await LoadStatusAsync();
    }

    private async Task LoadStatusAsync()
    {
        if (_viewModel is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        RefreshButton.IsEnabled = false;

        await _viewModel.LoadCommand.ExecuteAsync(null);

        UpdateDisplay();

        // MLS status will be queried via nest HTTP API when available

        LoadProgress.IsActive = false;
        LoadProgress.Visibility = Visibility.Collapsed;
        RefreshButton.IsEnabled = true;
    }

    private void ViewModel_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_viewModel is null) return;

        switch (e.PropertyName)
        {
            case nameof(StatusViewModel.ErrorMessage):
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
            case nameof(StatusViewModel.IsLoading):
                LoadProgress.IsActive = _viewModel.IsLoading;
                LoadProgress.Visibility = _viewModel.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(StatusViewModel.NoticeMessage):
                CopyToast.Text = _viewModel.NoticeMessage ?? "";
                CopyToast.Visibility = string.IsNullOrEmpty(_viewModel.NoticeMessage)
                    ? Visibility.Collapsed : Visibility.Visible;
                break;
        }
    }

    private void UpdateDisplay()
    {
        if (_viewModel is null) return;

        // Connection state — use sync connection state
        var (connColor, connLabel) = _viewModel.SyncConnectionState switch
        {
            ConnectionState.Connected => (Colors.Green, S.Get("common/connected")),
            ConnectionState.Connecting => (Colors.Gold, S.Get("common/connecting")),
            ConnectionState.Disconnected => (Colors.Red, S.Get("common/disconnected")),
            _ => (Colors.Gray, S.Get("common/unknown")),
        };
        ConnectionDot.Fill = new SolidColorBrush(connColor);
        ConnectionText.Text = connLabel;

        // Service info
        VersionText.Text = _viewModel.SyncVersion ?? "--";
        UptimeText.Text = ValueFormat.DurationSecs(_viewModel.SyncUptimeSecs);
        ActorIdText.Text = _viewModel.ActorId ?? "--";
        HandleText.Text = _viewModel.Handle ?? "--";

        // Sync summary
        var syncConnected = _viewModel.SyncConnected;
        var syncing = _viewModel.Syncing;
        if (syncing)
        {
            SyncDot.Fill = new SolidColorBrush(Colors.DodgerBlue);
            SyncStatusText.Text = S.Get("status/sync/syncing");
        }
        else if (syncConnected)
        {
            SyncDot.Fill = new SolidColorBrush(Colors.Green);
            SyncStatusText.Text = S.Get("common/connected");
        }
        else
        {
            SyncDot.Fill = new SolidColorBrush(Colors.Red);
            SyncStatusText.Text = S.Get("common/disconnected");
        }

        // `status-sync-pending`: the shared `status/sync/pending_summary` template
        // over the file count and the localized byte size (status.md § State &
        // data shape — "the shared status.sync.pending_summary string over the
        // file count and fauna_core::format::byte_size"; windows resw is flat, so
        // the two placeholders are substituted here, same shape as
        // AdminMailPage's spam_baseline_published).
        FilesPendingText.Text = S.Get("status/sync/pending_summary")
            .Replace("{files}", _viewModel.FilesPending.ToString())
            .Replace("{bytes}", ValueFormat.ByteSize(_viewModel.BytesPending));
        LastSyncText.Text = StatusViewModel.LastSyncText(_viewModel.LastSync, DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());

        // Quota (folded in from the old SettingsPage so it lives on the Settings
        // shell's default Status sub-page — settings.md § Navigation model). The
        // VM pre-formats the "used / max" cells; this just projects them.
        QuotaInboxText.Text = _viewModel.QuotaInbox;
        QuotaStorageText.Text = _viewModel.QuotaStorage;
        QuotaDevicesText.Text = _viewModel.QuotaDevices;

        UpdateFeatureLimits();
        PaintRegion();
    }

    /// <summary>
    /// Paints the <c>settings-region-*</c> section from the shared plane's view
    /// (<see cref="RegionSettingsModel"/> — the strings; this method only places them,
    /// one element per id). Clear-and-rebuild of a panel this method owns outright,
    /// like <see cref="UpdateFeatureLimits"/>. Before the shell opened the plane (no
    /// view yet) the section says nothing is declared.
    /// </summary>
    private void PaintRegion()
    {
        RegionRowsPanel.Children.Clear();
        var model = RegionPlaneHost.View() is { } view
            ? RegionSettingsModel.From(view, uniffi.fauna_ffi.FaunaFfiMethods.FormatUnixLocal)
            : RegionSettingsModel.From(
                new uniffi.fauna_ffi.FfiRegionView(null, System.Array.Empty<uniffi.fauna_ffi.FfiRegionPolicyRow>(), null, false),
                uniffi.fauna_ffi.FaunaFfiMethods.FormatUnixLocal);

        RegionRowsPanel.Children.Add(RegionLine(Ids.SettingsRegionDeclared, model.DeclaredText));
        if (model.SourceText is { } source)
            RegionRowsPanel.Children.Add(RegionLine(Ids.SettingsRegionSource, source));
        if (model.NoPolicyText is { } none)
            RegionRowsPanel.Children.Add(new TextBlock { Text = none, TextWrapping = TextWrapping.Wrap });
        foreach (var policy in model.Policies)
        {
            var item = new StackPanel { Spacing = 2 };
            AutomationProperties.SetAutomationId(item, Ids.SettingsRegionPolicyItem);
            // A bare container with only an AutomationId is pruned from the UIA tree.
            AutomationProperties.SetName(item, policy.AuthorityText);
            item.Children.Add(RegionLine(Ids.SettingsRegionPolicyAuthority, policy.AuthorityText));
            item.Children.Add(RegionLine(Ids.SettingsRegionPolicyVersion, policy.VersionText));
            if (policy.NoticeText is { } notice)
                item.Children.Add(RegionLine(Ids.SettingsRegionInertNotice, notice));
            RegionRowsPanel.Children.Add(item);
        }
        if (model.LastCheckedText is { } checkedText)
            RegionRowsPanel.Children.Add(RegionLine(Ids.SettingsRegionLastChecked, checkedText));
        if (model.StaleWarningText is { } stale)
            RegionRowsPanel.Children.Add(RegionLine(Ids.SettingsRegionStaleWarning, stale));
    }

    private static TextBlock RegionLine(string id, string text)
    {
        var line = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap };
        AutomationProperties.SetAutomationId(line, id);
        return line;
    }

    /// <summary>
    /// Paints <c>feature-limits-section</c> from the VM's already-folded rows
    /// (dynamic-features.md § Transparency &amp; auditability). This method
    /// PAINTS; it decides nothing — every judgement (bounds, headroom,
    /// per-cell tier attribution, available/disabled) is
    /// <c>fauna_client_features</c>' output via the VM's read. Rebuilds
    /// <see cref="FeatureLimitsRowsPanel"/>'s children wholesale on every
    /// call — a plain <c>StackPanel</c> this method owns outright, safe to
    /// blind-clear-and-rebuild (mirrors <c>ConversationsPage.MessagesList</c>
    /// / <c>FeedPage</c>'s imperative panels — variable row/cell counts, not a
    /// fixed field set like Quota above).
    /// </summary>
    private void UpdateFeatureLimits()
    {
        if (_viewModel is null) return;
        var rows = _viewModel.FeatureLimitRows;
        if (rows is null)
        {
            // Not loaded yet (or the read failed) — stays hidden rather than
            // painting an empty/stale surface (the section's own Collapsed
            // default in XAML covers this state too; explicit for the
            // re-Refresh case).
            FeatureLimitsSection.Visibility = Visibility.Collapsed;
            return;
        }
        FeatureLimitsSection.Visibility = Visibility.Visible;

        FeatureLimitsRowsPanel.Children.Clear();
        // Deliberately NOT the "nothing restricts you" state: an unrestricted
        // member is still a row (features.md's own contract) — this text is
        // for a nest that carries the plane but reports zero gated members
        // at all (an excised build).
        FeatureLimitsEmptyText.Visibility = rows.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

        foreach (var row in rows)
        {
            var rowName = S.Resolve(row.@name);
            var rowPanel = new StackPanel { Spacing = 2 };
            AutomationProperties.SetAutomationId(rowPanel, Ids.FeatureLimitsRow);
            // A bare container with only an AutomationId is pruned from the UIA
            // tree (reference_winui_flaui_datatemplate_name) — name it, same as
            // FeedPage's quoted-post Border.
            AutomationProperties.SetName(rowPanel, rowName);

            var nameText = new TextBlock { Text = rowName, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };
            AutomationProperties.SetAutomationId(nameText, Ids.FeatureLimitsName);
            rowPanel.Children.Add(nameText);

            var statusText = new TextBlock { Text = S.Resolve(row.@status), Opacity = 0.7, FontSize = 12 };
            AutomationProperties.SetAutomationId(statusText, Ids.FeatureLimitsStatus);
            rowPanel.Children.Add(statusText);

            // Only when something actually blocks — boundary 4's "no silent
            // gates" half. resolve_nested, not Resolve: the sentence's
            // {window} is itself an i18n key.
            if (row.@restriction is { } restriction)
            {
                var restrictionText = new TextBlock
                {
                    Text = S.ResolveNested(restriction),
                    Opacity = 0.7,
                    FontSize = 12,
                    TextWrapping = TextWrapping.Wrap,
                };
                AutomationProperties.SetAutomationId(restrictionText, Ids.FeatureLimitsRestriction);
                rowPanel.Children.Add(restrictionText);
            }

            foreach (var cell in row.@cells)
            {
                var cellLabel = S.ResolveNested(cell.@label);
                var cellPanel = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
                AutomationProperties.SetAutomationId(cellPanel, Ids.FeatureLimitsQuota);
                AutomationProperties.SetName(cellPanel, cellLabel);

                var labelText = new TextBlock { Text = cellLabel, Opacity = 0.6, FontSize = 12 };
                AutomationProperties.SetAutomationId(labelText, Ids.FeatureLimitsQuotaLabel);
                cellPanel.Children.Add(labelText);

                // Per CELL, not per row: the meet takes the MIN per
                // (dimension, window), so two bounds on one feature can come
                // from different tiers.
                var valueText = new TextBlock { Text = S.CellValueText(cell), FontSize = 12 };
                AutomationProperties.SetAutomationId(valueText, Ids.FeatureLimitsQuotaValue);
                cellPanel.Children.Add(valueText);

                var tierText = new TextBlock { Text = S.Resolve(cell.@tierLabel), Opacity = 0.5, FontSize = 11 };
                AutomationProperties.SetAutomationId(tierText, Ids.FeatureLimitsQuotaTier);
                cellPanel.Children.Add(tierText);

                rowPanel.Children.Add(cellPanel);
            }

            FeatureLimitsRowsPanel.Children.Add(rowPanel);
        }
    }

    // "--" is the page's "no value yet" placeholder — skip it (the shared helper
    // already no-ops on null/empty).
    private async void CopyActorId_Click(object sender, RoutedEventArgs e)
    {
        if (ActorIdText.Text == "--") return;
        var copied = ActorIdText.Text;
        FaunaApp.Helpers.ClipboardHelper.CopyText(copied);
        // ui.yaml `status-actor-id-copy-btn`: the button carries a `copied` attr
        // holding the exact string it put on the clipboard, the same shape
        // ProfilePage.CopyActorId_Click uses (windows get_attr maps a non-`disabled`
        // name to HelpText). Written AFTER the copy from the same value.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(StatusActorIdCopyButton, copied);
        await ShowCopiedToastAsync();
    }

    private async void CopyNestUrl_Click(object sender, RoutedEventArgs e)
    {
        if (NestUrlText.Text == "--") return;
        var copied = NestUrlText.Text;
        FaunaApp.Helpers.ClipboardHelper.CopyText(copied);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(StatusNodeUrlCopyButton, copied);
        await ShowCopiedToastAsync();
    }

    /// <summary>Display the shared "Copied!" confirmation (<c>common/copied</c>) for
    /// two seconds, painted from <see cref="StatusViewModel.NoticeMessage"/> via
    /// <see cref="ViewModel_PropertyChanged"/> — <see cref="ViewModelBase.SetNotice"/>
    /// logs the displayed text at <c>info</c> on the same call (observability.md §
    /// What must be logged, category 1), never the copied value.</summary>
    private async Task ShowCopiedToastAsync()
    {
        if (_viewModel is null) return;
        _viewModel.SetNotice(S.Get("common/copied"));
        await Task.Delay(TimeSpan.FromSeconds(2));
        if (_viewModel is not null) _viewModel.SetNotice(null);
    }
}
