using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views;

/// <summary>
/// The admin <c>admin-dashboard</c> page — a dumb renderer of
/// <see cref="AdminDashboardViewModel"/>, which holds the shared card set
/// (<c>admin_dashboard_snapshot()</c>; admin.md § 1. Dashboard → <i>The card
/// set</i>). Re-reads on every entry: no client-side cache. Only accessible to
/// nest administrators.
/// </summary>
public sealed partial class AdminDashboardPage : Page
{
    private AdminDashboardViewModel? _vm;

    public AdminDashboardPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients { Rpc: { } rpc })
        {
            _vm ??= new AdminDashboardViewModel(rpc);
            CardsList.ItemsSource = _vm.Cards;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            await _vm.LoadAsync();
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
        ShowError(_vm.ErrorMessage);
    }

    private void ShowError(string? message)
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
}
