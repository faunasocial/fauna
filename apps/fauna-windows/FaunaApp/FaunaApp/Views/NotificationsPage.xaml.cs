using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

public sealed partial class NotificationsPage : Page
{
    private NotificationsViewModel? _viewModel;

    public NotificationsPage()
    {
        this.InitializeComponent();
        NotificationsTitle.Text = S.Get("common/notifications");
        MarkReadButton.Content = S.Get("bridges/mark_all_read");
        LoadMoreButton.Content = S.Get("common/load_more");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _viewModel = new NotificationsViewModel(clients.Rpc!);
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
        }
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        // Unsubscribe the VM's reconnect re-hydrate handler (the INestRpcClient
        // seam is app-lifetime, so an un-cleaned VM would leak).
        _viewModel?.CleanupReconnect();
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        await _viewModel.LoadCommand.ExecuteAsync(null);
        NotificationsList.ItemsSource = _viewModel.Notifications;
        LoadProgress.IsActive = false;
        LoadProgress.Visibility = Visibility.Collapsed;
        LoadMoreButton.Visibility = _viewModel.HasMore ? Visibility.Visible : Visibility.Collapsed;
        CountBadge.Text = _viewModel.UnreadCount > 0 ? $"{_viewModel.UnreadCount} unread" : "0 unread";
        // Keep the shell's app-global count (and the state snapshot it owns) in step
        // with what this page just read from the nest .
        MainPage.Current?.RefreshUnreadNotificationCount();
    }

    private void ViewModel_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_viewModel is null) return;
        switch (e.PropertyName)
        {
            case nameof(NotificationsViewModel.ErrorMessage):
                if (_viewModel.ErrorMessage is not null) { ErrorBar.Message = _viewModel.ErrorMessage; ErrorBar.IsOpen = true; App.CurrentErrorMessage = _viewModel.ErrorMessage; }
                else { ErrorBar.IsOpen = false; App.CurrentErrorMessage = null; }
                break;
            case nameof(NotificationsViewModel.UnreadCount):
                CountBadge.Text = _viewModel.UnreadCount > 0 ? $"{_viewModel.UnreadCount} unread" : "0 unread";
                break;
            case nameof(NotificationsViewModel.HasMore):
                LoadMoreButton.Visibility = _viewModel.HasMore ? Visibility.Visible : Visibility.Collapsed;
                break;
        }
    }

    private async void MarkRead_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is not null) await _viewModel.MarkReadCommand.ExecuteAsync(null);
        // Mark-all-read is a MUTATION of the very count the shell publishes, so the
        // shell has to re-read it — otherwise `data.notifications.unread_count` keeps
        // reporting the pre-mark value until the next push.
        MainPage.Current?.RefreshUnreadNotificationCount();
    }

    private async void LoadMore_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is not null) await _viewModel.LoadMoreCommand.ExecuteAsync(null);
    }
}
