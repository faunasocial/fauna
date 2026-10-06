using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Encryption sub-page (settings.md § Navigation model). MLS status +
/// key-package count. Split out of the former single-scroll
/// <see cref="SettingsPage"/>; constructs its own <see cref="SettingsViewModel"/>
/// from <see cref="ServiceClients"/> in OnNavigatedTo, mirroring the admin
/// sub-pages.
/// </summary>
public sealed partial class SettingsEncryptionPage : Page
{
    private SettingsViewModel? _viewModel;

    public SettingsEncryptionPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _viewModel = new SettingsViewModel(clients.Nest, clients.Rpc!);
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.LoadCommand.ExecuteAsync(null);
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
            case nameof(SettingsViewModel.IsMlsAvailable):
                MlsStatusText.Text = _viewModel.IsMlsAvailable ? S.Get("settings/mls_available") : S.Get("settings/mls_not_available");
                break;
            case nameof(SettingsViewModel.KeyPackageCount):
                KeyPackageCountText.Text = $"{S.Get("status/encryption/key_packages")}: {_viewModel.KeyPackageCount}";
                break;
        }
    }
}
