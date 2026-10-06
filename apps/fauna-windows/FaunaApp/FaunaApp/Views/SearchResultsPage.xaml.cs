using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Search;
using Windows.System;
using uniffi.fauna_ffi;
using uniffi.fauna_client_search;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Search page. Renders entirely off the shared <see cref="FfiSearchManager"/>
/// snapshot via <see cref="SearchViewModel"/> + a UI-thread
/// <see cref="SearchNotifyObserver"/> — query/type-filter/paging/merge state
/// all run in shared Rust (search.md § State &amp; data shape). The live query
/// buffer and clear-button state stay page-local (search.md § Where logic
/// lives — submit-driven, no debounce; <c>search-clear-button</c> not yet
/// manager-owned).
/// </summary>
public sealed partial class SearchResultsPage : Page
{
    private SearchViewModel? _viewModel;
    private SearchNotifyObserver? _observer;
    private INestRpcClient? _rpc;
    private ServiceClients? _clients;

    public SearchResultsPage()
    {
        this.InitializeComponent();
        SearchBox.PlaceholderText = S.Get("search_page/placeholder");
        LoadMoreButton.Content = S.Get("common/load_more");
        PopulateTypeFilter();
    }

    /// <summary>
    /// The type-filter tokens AND their labels come from the SAME shared
    /// mapping the manager applies to both search backends (search.md § State
    /// &amp; data shape — *Type filter*) — a native picker can never drift
    /// from it, and it retires the dead "file" tag the hand-rolled combo used
    /// to offer (not a real nest content_type; that option always returned
    /// zero rows). Label resolution goes through the shared
    /// <c>search_type_filter_label</c> face (the same one linux/web/android/
    /// apple/tui call), which is expressed over the same badge map a row's
    /// own <c>search-result-item</c> uses — so the picker can never read
    /// differently from its own results again. Each item's automation Name is
    /// its wire token, not its label — the windows picker convention
    /// (<c>FeedPage</c>'s <c>feed-rule-type-select</c>): an e2e selects the
    /// stable token every app paints while the user reads the localized label.
    /// </summary>
    private void PopulateTypeFilter()
    {
        TypeFilterCombo.Items.Clear();
        foreach (var token in FaunaFfiMethods.SearchTypeFilterOptions())
        {
            var label = S.Resolve(FaunaFfiMethods.SearchTypeFilterLabel(token));
            var item = new ComboBoxItem { Tag = token, Content = label };
            AutomationProperties.SetName(item, token);
            TypeFilterCombo.Items.Add(item);
        }
        if (TypeFilterCombo.Items.Count > 0) TypeFilterCombo.SelectedIndex = 0;
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
            _rpc = clients.Rpc;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_rpc is null) return;

        // Guard mirrors FeedPage.Page_Loaded: this is an async void event
        // handler, so an un-caught throw (a transient connect failure) would
        // vanish silently and leave the page with a null VM.
        try
        {
            _observer = new SearchNotifyObserver();
            var manager = await _rpc.BuildSearchManagerAsync();
            // Backend 2's arm — the sealed local index — attached before the page
            // ever queries, so the first search already merges local rows
            // (content-index.md § Where queries run — per app). A `false` return
            // is a normal state (no conversations session yet, or mail not
            // enabled) — this page rebuilds a fresh manager on every load, so a
            // page entered before mail was enabled simply retries next load.
            await _rpc.AttachLocalSearchIndexAsync(manager);
            _viewModel = new SearchViewModel(manager, _observer);
            _viewModel.PropertyChanged += (_, _) => Refresh();
            Refresh();
        }
        catch (System.Exception ex)
        {
            Core.Logs.ShellLog.Error(nameof(SearchResultsPage), $"search page load failed: {ex.Message}");
            var msg = Core.Services.Strings.Error(ex);
            ErrorBar.Message = msg;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
    }

    private void Refresh()
    {
        if (_viewModel is null) return;

        LoadingRing.IsActive = _viewModel.IsSearching;
        LoadingRing.Visibility = _viewModel.IsSearching ? Visibility.Visible : Visibility.Collapsed;
        SearchButton.IsEnabled = !_viewModel.IsSearching;

        ResultsList.ItemsSource = _viewModel.Results;
        NoResultsText.Text = S.Get("search_page/no_results");
        NoResultsText.Visibility = _viewModel.NoResults ? Visibility.Visible : Visibility.Collapsed;
        LoadMoreButton.Visibility = _viewModel.HasMore ? Visibility.Visible : Visibility.Collapsed;
        SearchCancelButton.Visibility = _viewModel.IsSearching ? Visibility.Visible : Visibility.Collapsed;

        var err = _viewModel.ErrorText;
        if (!string.IsNullOrEmpty(err))
        {
            ErrorBar.Message = err; ErrorBar.IsOpen = true; App.CurrentErrorMessage = err;
        }
        else { ErrorBar.IsOpen = false; App.CurrentErrorMessage = null; }
    }

    private async void SearchButton_Click(object sender, RoutedEventArgs e)
    {
        await DoSearch();
    }

    private async void SearchBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Enter) await DoSearch();
    }

    private async void TypeFilter_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_viewModel is null) return;
        var selected = TypeFilterCombo.SelectedItem as ComboBoxItem;
        var token = selected?.Tag as string ?? "all";
        // Re-fire only if a search already ran (the last-FIRED query, from the
        // snapshot — mirrors the pre-adoption gate), using the LIVE query-box
        // text (never the stale fired query) as search.md's RunQuery contract
        // requires.
        if (!string.IsNullOrWhiteSpace(_viewModel.Query))
        {
            await _viewModel.Manager.RunQuery(SearchBox.Text, token);
        }
    }

    private async void LoadMore_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.Manager.LoadMore();
    }

    private void SearchToggle_Click(object sender, RoutedEventArgs e)
    {
        SearchBox.Visibility = SearchBox.Visibility == Visibility.Visible
            ? Visibility.Collapsed
            : Visibility.Visible;
    }

    private void SearchClear_Click(object sender, RoutedEventArgs e)
    {
        SearchBox.Text = string.Empty;
    }

    private void SearchCancel_Click(object sender, RoutedEventArgs e)
    {
        _viewModel?.Manager.Cancel();
    }

    private async Task DoSearch()
    {
        if (_viewModel is null) return;
        var selected = TypeFilterCombo.SelectedItem as ComboBoxItem;
        var token = selected?.Tag as string ?? "all";
        await _viewModel.Manager.RunQuery(SearchBox.Text, token);
    }

    /// <summary>
    /// Activating a result row routes by its typed <c>SearchNav</c> target
    /// (search.md § User actions — "search-result-item[i] | Open
    /// destination"), the three-part contract every other app implements.
    /// An inert row (<c>Nav</c> null — search.md § The page's wire surface:
    /// profile/bridge rows stay non-navigable until the additive id field
    /// ships) is a silent no-op.
    /// </summary>
    private async void SearchResult_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: SearchResult result } || result.Nav is null) return;

        switch (result.Nav)
        {
            case SearchNav.Post post:
                Frame.Navigate(typeof(FeedPage), _clients! with { DeepLinkPostId = post.postId });
                break;

            case SearchNav.Mail mail:
                Frame.Navigate(typeof(ConversationsPage), _clients! with
                {
                    DeepLinkOpenConversations = true,
                    DeepLinkThreadId = mail.threadId,
                    DeepLinkMessageId = mail.messageId,
                });
                break;

            case SearchNav.Draft draft:
                Frame.Navigate(typeof(ConversationsPage), _clients! with
                {
                    DeepLinkOpenConversations = true,
                    DeepLinkThreadId = draft.threadId,
                });
                break;

            case SearchNav.Contact contact:
                await OpenContactAsync(contact.uidHash);
                break;

            case SearchNav.File file:
                Frame.Navigate(typeof(MediaPage), _clients! with
                {
                    DeepLinkFile = (file.folderId, file.pathHash),
                });
                break;
        }
    }

    /// <summary>
    /// Resolve a <c>SearchNav.Contact</c> hit's <c>uid_hash</c> to its card
    /// BEFORE navigating (unlike the Mail/Draft/File arms, which navigate
    /// first and resolve once their destination page's own machine has
    /// loaded): <see cref="Views.CardDetailPage"/> takes an already-decoded
    /// <see cref="CardInfo"/> directly off its nav args (the windows
    /// master→detail convention, <c>ContactsPage.CardItem_Click</c>) and owns
    /// no re-fetch of its own, so there is no "wait for the page to load"
    /// step to hook into. <c>found.cards</c> is the WHOLE holding book's card
    /// list (the same shape a books/cards read costs), so the located card is
    /// the entry whose <c>id</c> matches <c>found.card_id</c> — never
    /// <c>cards[0]</c> (linux's <c>address_book.rs</c> repopulate-from-locate
    /// reference). A <c>null</c> <c>found</c> or a missing match (deleted, or
    /// a stale index row — the DROPPED case, search.md § Implementation
    /// status today) deep-links to <see cref="ContactsPage"/>'s Address Book
    /// segment instead of a silent no-op: that page still has a book picker
    /// to repaint, which <see cref="SearchResultsPage"/> does not (mirrors
    /// tui's <c>Outcome::CardLocated</c> <c>None</c> arm and apple's
    /// <c>AddressBookVM.locateCard</c>).
    /// </summary>
    private async Task OpenContactAsync(string uidHash)
    {
        if (_rpc is null) return;
        try
        {
            var located = await _rpc.CarddavLocateCardByUidHashAsync(uidHash);
            if (located.found is { } found)
            {
                var row = found.cards.FirstOrDefault(c => c.id == found.cardId);
                if (row is not null)
                {
                    var card = CardInfo.FromFfi(row);
                    Frame.Navigate(typeof(CardDetailPage), new CardDetailNavigationArgs(card));
                    return;
                }
            }
            Frame.Navigate(typeof(ContactsPage), _clients! with { DeepLinkContactNotFound = true });
        }
        catch (Exception ex)
        {
            ShellLog.Warn(nameof(SearchResultsPage), $"Contact search-nav resolve failed: {ex.Message}");
        }
    }
}
