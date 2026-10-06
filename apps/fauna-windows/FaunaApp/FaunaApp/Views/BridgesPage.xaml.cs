using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Controls;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

public sealed partial class BridgesPage : Page
{
    private BridgesViewModel? _viewModel;

    public BridgesPage()
    {
        this.InitializeComponent();
        BridgesTitle.Text = S.Get("common/bridges");
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
}
