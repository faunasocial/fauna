using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Services;
using uniffi.fauna_ffi;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>Picks the Button-root row for a notification with a destination, the plain
/// text row for one without.</summary>
public sealed class NotificationRowTemplateSelector : DataTemplateSelector
{
    public DataTemplate? Navigable { get; set; }
    public DataTemplate? Inert { get; set; }

    protected override DataTemplate? SelectTemplateCore(object item)
        => item is NotificationItem { IsNavigable: true } ? Navigable : Inert;

    protected override DataTemplate? SelectTemplateCore(object item, DependencyObject container)
        => SelectTemplateCore(item);
}

public sealed partial class NotificationsPage : Page
{
    private NotificationsViewModel? _viewModel;
    private ServiceClients? _clients;

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
            _clients = clients;
            _viewModel = new NotificationsViewModel(clients.Rpc!, CanNavigate);
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

    /// <summary>
    /// What this app can act on right now. Exhaustive over the shared enum on purpose
    /// (no default arm): a new variant must be decided here, not silently painted as a
    /// dead control. <c>Family</c> also depends on session state — the gated
    /// <c>family-tab</c> may not be shown (a ward that graduated away), and a row that
    /// outlived its link would otherwise navigate to a page with no sidebar entry.
    /// </summary>
    private static bool CanNavigate(FfiNotificationDestination destination)
    {
        switch (destination)
        {
            case FfiNotificationDestination.Post: return true;
            case FfiNotificationDestination.Knock: return true;
            case FfiNotificationDestination.Family: return MainPage.Current?.IsFamilyTabAvailable == true;
            case FfiNotificationDestination.External: return true;
        }
        return false;
    }

    /// <summary>
    /// Tapping a row routes the shared router's typed target into the destination
    /// page's own navigation (<c>notifications.md</c> § Deep-link destinations) — the
    /// app navigates, shared Rust decides. No <c>notif_type</c> table lives here.
    /// </summary>
    private void NotificationItem_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: NotificationItem { Destination: { } destination } } || _clients is null)
            return;

        switch (destination)
        {
            // Same door a search result's Post arm uses: FeedPage opens the detail
            // dialog, resolving a post the timeline never loaded.
            case FfiNotificationDestination.Post post:
                Frame.Navigate(typeof(FeedPage), _clients with { DeepLinkPostId = post.postId });
                break;

            // Contacts opens on the People segment, which lists the pending knocks.
            case FfiNotificationDestination.Knock:
                Frame.Navigate(typeof(ContactsPage), _clients);
                break;

            case FfiNotificationDestination.Family:
                Frame.Navigate(typeof(FamilyPage), _clients);
                break;

            // Off-app: the OS default browser through the one shared opener.
            case FfiNotificationDestination.External external:
                UrlOpener.Open(external.url, nameof(NotificationsPage));
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
