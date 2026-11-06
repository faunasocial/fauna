using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Controls;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;
using FaunaApp.UiIds;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

public sealed partial class BridgesPage : Page
{
    private BridgesViewModel? _viewModel;

    public BridgesPage()
    {
        this.InitializeComponent();
        BridgesTitle.Text = S.Get("common/bridges");
        PluginsHeading.Text = S.Get("bridges/plugins_heading");
        PluginsEmpty.Text = S.Get("bridges/plugins_empty");
        PluginInstallUrlBox.Header = S.Get("bridges/plugin_install_label");
        PluginInstallButton.Content = S.Get("bridges/plugin_install_submit");
        PluginInstallOpenedText.Text = S.Get("bridges/plugin_install_opened");
        PluginBindOpenedText.Text = S.Get("bridges/plugin_bind_opened");
        // The install is the admin's online-only call; the row buttons are declared as
        // their rows are built (RenderPlugins), since rows are rebuilt on every render.
        PluginInstallButton.FaunaGate("fauna.plugins.install");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _viewModel = new BridgesViewModel(clients.Rpc!);
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.LoadCommand.ExecuteAsync(null);
        BridgesList.ItemsSource = _viewModel.Bridges;
        await _viewModel.LoadPluginCardsAsync();
        RenderPluginCards();
        await _viewModel.LoadPluginsAsync();
        RenderPlugins();
    }

    // ── Every account's plugin settings cards (third-party.md § The principal model →
    //    Hosted principals) ──

    /// <summary>Paint one more <c>bridge-card</c> per installed plugin, after the bridges —
    /// nothing until the read returned — then the bind hint after a Connect. Reference
    /// painter: tui <c>bridges.rs::plugin_card_elements</c>.</summary>
    private void RenderPluginCards()
    {
        if (_viewModel is null) return;
        PluginCardsHost.Children.Clear();
        foreach (var card in _viewModel.PluginCards ?? System.Array.Empty<uniffi.fauna_ffi.FfiPluginCard>())
        {
            PluginCardsHost.Children.Add(BuildPluginCard(card));
        }
        PluginBindOpenedText.Visibility =
            _viewModel.PluginBindOpened ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>One plugin's settings card as a <c>bridge-card</c>: a Grid root carrying the
    /// card's AutomationId and a non-empty Name (a code-built row without one counts as zero),
    /// the plugin's name, its badge and publisher, its <c>bridge-action-button</c> —
    /// <i>Connect</i> while unbound, <i>Disconnect</i> while bound, with the plugin's
    /// <c>client_id</c> and <c>bound</c> packed into its HelpText so a reader can tell which
    /// card is which plugin's — and, once bound, its settings through the one settings
    /// renderer (<see cref="BridgeSettingRows"/>), each editable and written by
    /// <c>fauna.plugins.set_settings</c>.</summary>
    private FrameworkElement BuildPluginCard(uniffi.fauna_ffi.FfiPluginCard card)
    {
        var root = new Grid { Padding = new Thickness(12), ColumnSpacing = 12 };
        root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        root.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        for (var i = 0; i < 3; i++) root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        AutomationProperties.SetAutomationId(root, Ids.BridgeCard);
        AutomationProperties.SetName(root, card.name);

        var name = new TextBlock
        {
            Text = card.name,
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
        };
        root.Children.Add(name);

        var subtitle = new TextBlock
        {
            Text = BridgesViewModel.PluginCardSubtitle(card),
            Opacity = 0.6,
            TextWrapping = TextWrapping.Wrap,
        };
        Grid.SetRow(subtitle, 1);
        root.Children.Add(subtitle);

        var action = new Button
        {
            Content = new TextBlock { Text = BridgesViewModel.PluginCardActionLabel(card) },
            VerticalAlignment = VerticalAlignment.Center,
            Tag = card,
        };
        AutomationProperties.SetAutomationId(action, Ids.BridgeActionButton);
        AutomationProperties.SetHelpText(action, BridgesViewModel.PluginCardActionAttributes(card));
        action.Click += PluginCardAction_Click;
        if (card.bound) action.FaunaGate("fauna.principals.revoke");
        else action.FaunaGate("fauna.plugins.bind");
        Grid.SetColumn(action, 1);
        Grid.SetRowSpan(action, 2);
        root.Children.Add(action);

        // The settings are the bound account's own values; unbound there are none.
        var settings = new StackPanel { Spacing = 4, Margin = new Thickness(0, 8, 0, 0) };
        var principalIdHex = card.principalIdHex;
        foreach (var setting in BridgesViewModel.PluginCardSettings(card))
        {
            settings.Children.Add(BridgeSettingRows.Build(
                setting,
                value => PluginSetting_Committed(principalIdHex, value),
                editor => editor.FaunaGate("fauna.plugins.set_settings")));
        }
        Grid.SetRow(settings, 2);
        Grid.SetColumnSpan(settings, 2);
        root.Children.Add(settings);
        return root;
    }

    private async void PluginCardAction_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || (sender as FrameworkElement)?.Tag is not uniffi.fauna_ffi.FfiPluginCard card) return;
        await _viewModel.PressPluginCardAsync(card);
        RenderPluginCards();
    }

    private async void PluginSetting_Committed(string principalIdHex, BridgeSettingValue value)
    {
        if (_viewModel is null) return;
        await _viewModel.SetPluginSettingAsync(principalIdHex, value);
        RenderPluginCards();
    }

    // ── The admin's hosted plugins (bridges.md § Hosted plugins on this page) ──

    /// <summary>Project the VM's plugin state onto the footer section: hidden unless the
    /// admin gate passed; then the three-state list (nothing until the read returned, then
    /// rows or the empty line), the install entry and its hint. Reference painter: tui
    /// <c>bridges.rs::plugin_elements</c>.</summary>
    private void RenderPlugins()
    {
        if (_viewModel is null) return;
        PluginsSection.Visibility = _viewModel.IsAdmin ? Visibility.Visible : Visibility.Collapsed;
        PluginsHost.Children.Clear();
        var plugins = _viewModel.Plugins;
        PluginsEmpty.Visibility = plugins is { Count: 0 } ? Visibility.Visible : Visibility.Collapsed;
        foreach (var row in plugins ?? System.Array.Empty<uniffi.fauna_ffi.FfiInstalledPlugin>())
        {
            PluginsHost.Children.Add(BuildPluginRow(row));
        }
        PluginInstallOpenedText.Visibility =
            _viewModel.PluginInstallOpened ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>One <c>bridge-plugin-item</c>: a Grid root carrying the item's AutomationId and
    /// a non-empty Name (without one FlaUI counts a code-built row as zero), with its leaves
    /// scoped inside it — the name (<c>client_id</c> attr), the publisher, the runner status
    /// (<c>state</c> attr; both attrs ride HelpText, which the windows bridge maps any non-name
    /// attr to) and Uninstall. The first Uninstall press arms the inline confirm: the prompt,
    /// the same <c>bridge-plugin-uninstall</c> reading <i>Uninstall now</i>, and an untagged
    /// <i>Keep</i>.</summary>
    private FrameworkElement BuildPluginRow(uniffi.fauna_ffi.FfiInstalledPlugin row)
    {
        var item = new Grid
        {
            Padding = new Thickness(12),
            RowSpacing = 6,
            BorderBrush = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"],
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(6),
        };
        AutomationProperties.SetAutomationId(item, Ids.BridgePluginItem);
        AutomationProperties.SetName(item, "plugin");

        var name = new TextBlock { Text = row.name, Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"] };
        AutomationProperties.SetAutomationId(name, Ids.BridgePluginName);
        AutomationProperties.SetHelpText(name, row.clientId);
        AddRow(item, name);

        var publisher = new TextBlock
        {
            Text = BridgesViewModel.PluginPublisherText(row),
            Opacity = 0.7,
            TextWrapping = TextWrapping.Wrap,
        };
        AutomationProperties.SetAutomationId(publisher, Ids.BridgePluginPublisher);
        AddRow(item, publisher);

        var status = new TextBlock { Text = BridgesViewModel.PluginStatusText(row), TextWrapping = TextWrapping.Wrap };
        AutomationProperties.SetAutomationId(status, Ids.BridgePluginStatus);
        AutomationProperties.SetHelpText(status, row.state);
        AddRow(item, status);

        var armed = _viewModel?.PluginUninstallArmed == row.principalIdHex;
        if (armed)
        {
            AddRow(item, new TextBlock
            {
                Text = S.Get("bridges/plugin_uninstall_prompt").Replace("{name}", row.name),
                TextWrapping = TextWrapping.Wrap,
            });
        }
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        var uninstall = new Button
        {
            Content = S.Get(armed ? "bridges/plugin_uninstall_confirm" : "bridges/plugin_uninstall"),
            Tag = row.principalIdHex,
        };
        AutomationProperties.SetAutomationId(uninstall, Ids.BridgePluginUninstall);
        uninstall.Click += PluginUninstall_Click;
        actions.Children.Add(uninstall);
        if (armed)
        {
            var keep = new Button { Content = S.Get("bridges/plugin_uninstall_cancel") };
            keep.Click += PluginUninstallCancel_Click;
            actions.Children.Add(keep);
            // The armed press is the commit.
            uninstall.FaunaGate("fauna.plugins.uninstall");
        }
        AddRow(item, actions);
        return item;
    }

    private static void AddRow(Grid grid, FrameworkElement child)
    {
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        Grid.SetRow(child, grid.RowDefinitions.Count - 1);
        grid.Children.Add(child);
    }

    private void PluginInstallUrl_TextChanged(object sender, TextChangedEventArgs e) =>
        PluginInstallButton.IsEnabled = PluginInstallUrlBox.Text.Trim().Length > 0;

    private async void PluginInstall_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        if (await _viewModel.InstallPluginAsync(PluginInstallUrlBox.Text))
            PluginInstallUrlBox.Text = string.Empty;
        RenderPlugins();
    }

    private async void PluginUninstall_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || (sender as FrameworkElement)?.Tag is not string principalIdHex) return;
        await _viewModel.PressUninstallPluginAsync(principalIdHex);
        RenderPlugins();
    }

    private void PluginUninstallCancel_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        _viewModel.CancelUninstallPlugin();
        RenderPlugins();
    }

    private void ViewModel_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_viewModel is null) return;
        switch (e.PropertyName)
        {
            case nameof(BridgesViewModel.IsLoading):
                LoadingRing.IsActive = _viewModel.IsLoading;
                LoadingRing.Visibility = _viewModel.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(BridgesViewModel.ErrorMessage):
                if (_viewModel.ErrorMessage is not null)
                {
                    ErrorBar.Message = _viewModel.ErrorMessage;
                    ErrorBar.IsOpen = true;
                    App.CurrentErrorMessage = _viewModel.ErrorMessage;
                }
                else { ErrorBar.IsOpen = false; App.CurrentErrorMessage = null; }
                break;
        }
    }

    /// <summary>A card's action button was pressed. The shared
    /// <see cref="BridgeCard"/> hands over the rendered <see cref="BridgeInfo"/>
    /// itself, so this no longer looks the row up by id — and the link/unlink
    /// ceremony itself now lives in <see cref="BridgeCardActions"/>, shared with the
    /// AT Protocol page's Linked-account panel (the second host of the same card).</summary>
    private async void BridgeAction_Click(object? sender, BridgeInfo bridge)
    {
        if (_viewModel is null) return;
        await BridgeCardActions.RunAsync(this.XamlRoot, _viewModel, bridge);
    }

    /// <summary>A card's metadata-driven settings row committed a new value
    /// (bridges.md § Bridge settings) — dispatch and
    /// re-read, mirroring <see cref="BridgeAction_Click"/>'s own act-then-reload
    /// shape.</summary>
    private async void BridgeSetting_Changed(object? sender, BridgeSettingChangedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.SetSettingAsync(e.BridgeId, e.Value);
    }

    /// <summary>The ward pressed <c>bridge-source-request-button</c> on a card for ONE
    /// refused link/follow (family-safety.md § Feed-source approvals → App affordance).
    /// The card hands over that row's own triple; the durable pending row arrives with
    /// the status re-read the ask triggers, and the card repaints itself.</summary>
    private async void BridgeSourceAsk_Click(object? sender, SourceAskEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.AskSourceAsync(e.Triple, e.Label);
    }

    /// <summary>Approve or Refuse on one of a card's follow requests (bridges.md
    /// § Follow requests): answered and re-read by the view model — the card then
    /// repaints from what the nest still lists, never from the tap.</summary>
    private async void BridgeFollowRequest_Answer(object? sender, FollowRequestAnswerEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.AnswerFollowRequestAsync(e.BridgeId, e.Id, e.Approve);
    }

    /// <summary>A card's add row asked to follow (bridges.md § User actions): added and
    /// re-read by the view model.</summary>
    private async void BridgeFollow_Add(object? sender, FollowAddEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.AddFollowAsync(e.BridgeId, e.Id, e.Petname);
    }

    /// <summary>A card's follow row asked to be removed: removed and re-read.</summary>
    private async void BridgeFollow_Remove(object? sender, FollowRemoveEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.RemoveFollowAsync(e.BridgeId, e.FollowId);
    }
}
