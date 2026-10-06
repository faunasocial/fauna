using System.Linq;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Services;

namespace FaunaApp.Views;

/// <summary>
/// Displays knocks (pending contact requests) and the contact list
/// with status badges. Contact + add-contact (knock-send) operations ride the
/// WS-RPC seam (<see cref="INestRpcClient"/>); the VM keeps the HTTP client only
/// for actor search.
/// </summary>
public sealed partial class ContactsPage : Page
{
    private ContactsViewModel? _viewModel;
    private INestRpcClient? _rpc;
    /// <summary>Set from <see cref="ServiceClients.DeepLinkContactNotFound"/> —
    /// consumed once in <see cref="Page_Loaded"/> (see <see cref="ShowContactNotFoundAsync"/>).</summary>
    private bool _deepLinkContactNotFound;

    public ContactsPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _rpc = clients.Rpc;
            _viewModel = new ContactsViewModel(clients.Rpc!);
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
            _deepLinkContactNotFound = clients.DeepLinkContactNotFound;

            // Refresh the knocks roster on each inbound knock push (re-homed from the
            // dead WebSocketService onto the WS-RPC knock pump). App-lifetime event →
            // unsubscribed in OnNavigatedFrom so it doesn't leak the page.
            if (_rpc is not null)
            {
                _rpc.KnockReceived += OnKnockReceived;
                // The Address Book's live refresh (transport.md § Push events,
                // `fauna.addressbook.changed`) + its reconnect re-pull backstop. Same
                // lifetime discipline as the knock handler: unsubscribed in
                // OnNavigatedFrom.
                _rpc.AddressBookPushChanged += OnAddressBookPushChanged;
                _rpc.Reconnected += OnAddressBookReconnected;
            }
        }
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        // Unsubscribe the knock-roster handler + the VM's reconnect re-hydrate
        // handler (the INestRpcClient seam is app-lifetime, so leaving them attached
        // would leak this page).
        if (_rpc is not null)
        {
            _rpc.KnockReceived -= OnKnockReceived;
            _rpc.AddressBookPushChanged -= OnAddressBookPushChanged;
            _rpc.Reconnected -= OnAddressBookReconnected;
        }
        _viewModel?.CleanupReconnect();
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;

        await _viewModel.LoadCommand.ExecuteAsync(null);

        KnocksList.ItemsSource = _viewModel.Knocks;
        ContactsList.ItemsSource = _viewModel.Contacts;
        AppDataSnapshot.SetContacts(_viewModel.Contacts.Select(c =>
            new AppDataSnapshot.ContactSnapshot(c.ActorId, c.Handle, c.Status.ToString())));
        AppDataSnapshot.SetKnocks(_viewModel.Knocks.Select(k =>
            new AppDataSnapshot.KnockSnapshot(k.ActorId, k.Summary, k.Timestamp)));

        NoKnocksText.Visibility = _viewModel.Knocks.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        NoMatchesText.Visibility = Visibility.Collapsed;

        LoadProgress.IsActive = false;
        LoadProgress.Visibility = Visibility.Collapsed;

        if (_deepLinkContactNotFound)
        {
            _deepLinkContactNotFound = false;
            await ShowContactNotFoundAsync();
        }
    }

    /// <summary>
    /// DROPPED case (search.md § Implementation status today, the Contact
    /// bullet): a search-result Contact hit whose card no longer exists by
    /// the time it's activated. Opens the Address Book segment — repainting
    /// whatever book picker rows still exist — and surfaces the ratified
    /// <c>card_not_found</c> string on the page's <c>error-message</c>
    /// element, mirroring every other app's leg for this case.
    /// </summary>
    private async System.Threading.Tasks.Task ShowContactNotFoundAsync()
    {
        PeopleView.Visibility = Visibility.Collapsed;
        AddressBookView.Visibility = Visibility.Visible;
        if (!_addressBooksLoaded)
        {
            _addressBooksLoaded = true;
            await LoadAddressbooksAsync();
        }
        ErrorBar.Message = Strings.Get("contacts/address_book/card_not_found");
        ErrorBar.IsOpen = true;
        App.CurrentErrorMessage = ErrorBar.Message;
    }

    private void ViewModel_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_viewModel is null) return;

        switch (e.PropertyName)
        {
            case nameof(ContactsViewModel.ErrorMessage):
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
            case nameof(ContactsViewModel.IsLoading):
                LoadProgress.IsActive = _viewModel.IsLoading;
                LoadProgress.Visibility = _viewModel.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
        }
    }

    private async void AcceptKnock_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        if (sender is Button btn && btn.Tag is string actorId)
        {
            await _viewModel.AcceptKnockCommand.ExecuteAsync(actorId);
            NoKnocksText.Visibility = _viewModel.Knocks.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        }
    }

    private async void BlockKnock_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        if (sender is Button btn && btn.Tag is string actorId)
        {
            await _viewModel.BlockKnockCommand.ExecuteAsync(actorId);
            NoKnocksText.Visibility = _viewModel.Knocks.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        }
    }

    private async void DismissKnock_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        if (sender is Button btn && btn.Tag is string actorId)
        {
            await _viewModel.DismissKnockCommand.ExecuteAsync(actorId);
            NoKnocksText.Visibility = _viewModel.Knocks.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        }
    }

    private void ContactSearch_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args)
    {
        if (_viewModel is null) return;
        // contacts-search-field is a LOCAL roster filter (matching linux
        // views/contacts/list.rs), not a nest search — full-text search returns opaque
        // FTS keys, not actor ids. The match predicate is the shared
        // fauna_core::format::contact_matches_filter (handle + domain + actor-id), via
        // ContactsViewModel.FilterRoster. Live-as-you-type (TextChanged, not
        // QuerySubmitted — an AutoSuggestBox's QuerySubmitted only fires on Enter/a
        // suggestion pick, never on a plain ValuePattern.SetValue, so a
        // submit-triggered filter here was structurally undrivable by the e2e harness
        // AND diverged from web/linux/apple's own live-substring contract).
        var query = (sender.Text ?? string.Empty).Trim();
        if (query.Length == 0)
        {
            ContactsList.ItemsSource = _viewModel.Contacts;
            NoMatchesText.Visibility = Visibility.Collapsed;
            return;
        }
        var filtered = ContactsViewModel.FilterRoster(_viewModel.Contacts, query);
        ContactsList.ItemsSource = filtered;
        // case (c) search-no-results (contacts.md § Errors & edge cases): only when a
        // NON-EMPTY roster narrowed to zero — never the true-empty-roster state.
        NoMatchesText.Visibility = _viewModel.Contacts.Count > 0 && filtered.Count == 0
            ? Visibility.Visible
            : Visibility.Collapsed;
    }

    private async void LookUpContact_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;

        // Federated find-user: classify + resolve the typed 64-hex actor-id | user@domain
        // handle in the VM (LookUpRecipientAsync over the shared classify_recipient /
        // resolve_nest / resolve_handle FFI seam), then surface the single resolved result
        // for add. contacts.md § Where logic lives → Contact lookup by handle.
        AddContactError.IsOpen = false;
        AddContactSuccess.IsOpen = false;
        SearchResultsList.Visibility = Visibility.Collapsed;

        await _viewModel.LookUpRecipientCommand.ExecuteAsync(AddContactActorId.Text);

        if (_viewModel.ActorIdError is not null)
        {
            AddContactError.Message = _viewModel.ActorIdError;
            AddContactError.IsOpen = true;
            return;
        }

        if (_viewModel.ActorIdResult is ContactInfo result)
        {
            SearchResultsList.ItemsSource = new[] { result };
            SearchResultsList.Visibility = Visibility.Visible;
        }
    }

    /// <summary>
    /// Maps ContactStatus to a colored brush for the status badge dot. The status
    /// <b>color</b> is an idiomatic per-app render (contacts.md § Where logic
    /// lives → Status badge text — only the label text is shared Rust). Confirmed is
    /// a mutually-confirmed, good-standing edge, so it reads green like Accepted.
    /// </summary>
    public static SolidColorBrush StatusToBrush(ContactStatus status)
    {
        return status switch
        {
            ContactStatus.Accepted => new SolidColorBrush(Colors.Green),
            ContactStatus.Confirmed => new SolidColorBrush(Colors.SeaGreen),
            ContactStatus.Pending => new SolidColorBrush(Colors.Gold),
            ContactStatus.Blocked => new SolidColorBrush(Colors.Red),
            _ => new SolidColorBrush(Colors.Gray),
        };
    }

    // The status label text is single-sourced in shared Rust
    // (fauna_core::format::contact_status_label) and rendered via the get-only
    // ContactInfo.StatusLabel that the XAML binds directly — the former
    // per-app StatusToLabel switch (which lacked a Confirmed arm and rendered
    // "Unknown") was removed (contacts.md § Where logic lives → Status badge text).

    private void CopyContactActorId_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn && btn.Tag is string actorId)
        {
            FaunaApp.Helpers.ClipboardHelper.CopyText(actorId);
        }
    }

    /// <summary>Renders `contact-confirm` iff the row's edge is `Accepted` — the
    /// ratified permanent target (contacts.md § Implementation status today,
    /// IN-PERSON ruling 2026-08-15, executed tui/linux 2026-08-17), not an
    /// interim shape. Confirmed rows have nothing left to confirm; Pending/
    /// Blocked rows have no accepted edge to promote.</summary>
    public static Visibility StatusToConfirmVisibility(ContactStatus status) =>
        status == ContactStatus.Accepted ? Visibility.Visible : Visibility.Collapsed;

    private async void ContactConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        if (sender is Button btn && btn.Tag is string actorId)
        {
            await _viewModel.ConfirmContactCommand.ExecuteAsync(actorId);
        }
    }

    /// <summary>Tap a contact row to open that peer's profile (OTHER profile — the
    /// follow button + offered tiers). Stashes the target for ProfilePage and
    /// navigates to the profile view, matching linux <c>app.rs</c> <c>open_profile</c>
    /// (contacts.md list → profile.md detail).
    ///
    /// Rides SelectionChanged (not ItemClick) so the e2e/UIA SelectionItem.Select()
    /// path drives the nav without a physical mouse click — mirrors ConversationsPage
    /// (an ItemClick tap needs SendInput, which the FlaUI bridge can't drive when the
    /// window isn't foreground: "SendInput: Access is denied"). Selection is cleared
    /// immediately so (a) no stale highlight lingers on return and (b) tapping the same
    /// row again re-fires (SelectionChanged only fires on change). The per-row buttons
    /// handle their own taps; clicking one doesn't change ListView selection.</summary>
    private void ContactsList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (ContactsList.SelectedItem is ContactInfo contact && !string.IsNullOrEmpty(contact.ActorId))
        {
            App.PendingProfileTarget = contact.ActorId;
            // Clear before navigating; the re-entrant SelectionChanged (now null)
            // no-ops on the type guard above.
            ContactsList.SelectedItem = null;
            Views.MainPage.Current?.NavigateToView("profile");
        }
    }

    /// <summary>Stamp each materialized <see cref="ListViewItem"/> container with the
    /// peer's actor-id as its AutomationId, so the unified e2e
    /// <c>open_contact_profile(hex)</c> resolves the row by that id (bridge Strategy 1
    /// ByAutomationId) onto a container that supports the UIA SelectionItem pattern —
    /// letting the bridge Select() it without a physical click. WinUI collapses a
    /// DataTemplate Grid's own AutomationId into its container, so the id must be set on
    /// the container itself (same reason ConversationsPage stamps <c>conversation-item</c>
    /// here). The inner Grid keeps its <c>contact-row</c> indexed id (it has a Name, so it
    /// still surfaces as its own peer).</summary>
    private void ContactsList_ContainerContentChanging(
        Microsoft.UI.Xaml.Controls.ListViewBase sender,
        Microsoft.UI.Xaml.Controls.ContainerContentChangingEventArgs args)
    {
        if (args.ItemContainer is Microsoft.UI.Xaml.Controls.ListViewItem container
            && args.Item is ContactInfo contact
            && !string.IsNullOrEmpty(contact.ActorId))
        {
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(container, contact.ActorId);
        }

        // contact-unattested-mark (succession-aftermath.md § Propagation) —
        // the trivial "is this actor id in the cached roster" join, inlined
        // rather than exported: android's identical contacts-badge check does
        // the same, since (unlike the thread-chip join) there is no MLS
        // address to resolve — a contact already carries a concrete actor id.
        if (args.ItemContainer.ContentTemplateRoot is Microsoft.UI.Xaml.FrameworkElement root
            && args.Item is ContactInfo c
            && root.FindName("UnattestedMarkText") is Microsoft.UI.Xaml.Controls.TextBlock mark)
        {
            var reviewed = _viewModel is not null
                && _viewModel.MemberReviewRoster.Any(r =>
                    System.Convert.ToHexString(r.person).Equals(c.ActorId, System.StringComparison.OrdinalIgnoreCase));
            mark.Visibility = reviewed ? Microsoft.UI.Xaml.Visibility.Visible : Microsoft.UI.Xaml.Visibility.Collapsed;
        }
    }

    private async void AddFoundContact_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        if (sender is Button btn && btn.Tag is string actorId)
        {
            // Knock the RESOLVED actor id (the result row's ActorId), not the typed
            // input, over fauna.inbox.send (AddFoundContactAsync → SendKnockAsync +
            // roster reload). A send failure surfaces on the page ErrorBar via the VM's
            // ShowError; on success confirm + collapse the resolved result.
            await _viewModel.AddFoundContactCommand.ExecuteAsync(actorId);
            if (_viewModel.ErrorMessage is not null) return;
            AddContactSuccess.IsOpen = true;
            SearchResultsList.Visibility = Visibility.Collapsed;
            AddContactActorId.Text = string.Empty;
        }
    }

    private void OnKnockReceived(uniffi.fauna_ffi.FfiKnock knock)
    {
        DispatcherQueue.TryEnqueue(async () =>
        {
            if (_viewModel is not null)
                await _viewModel.LoadCommand.ExecuteAsync(null);
        });
    }

    // ── Address Book segment (CardDAV vCards — read-only, slice 4b) ──────
    // A SEPARATE store from the social contact graph above (carddav-server.md
    // § Independent enablement; contacts.md § Address Book segment). Reads ride
    // the same page-lifetime INestRpcClient (_rpc) directly — no dedicated VM,
    // matching the read-only simplicity of the linux/web reference (no writes,
    // no reactive state to manage beyond the two lists).

    /// <summary>Lazily loaded on first switch to the Address Book segment (web
    /// parity: <c>showAddressBook</c> fetches only once per page visit).</summary>
    private bool _addressBooksLoaded;

    /// <summary>Which book's cards the list shows: a re-list keeps it open, and a
    /// card reply for any other book is a late reply for a book the user has left
    /// (transport.md § Push events; the linux <c>open_book</c> contract).</summary>
    private readonly AddressBookOpenBook _openBook = new();

    // A contacts app's first sync is one push per card, so a burst of pushes must not
    // stack overlapping re-reads: one runs, and any push arriving meanwhile queues a
    // single trailing re-read so the page ends on the newest state.
    private bool _bookRefreshRunning;
    private bool _bookRefreshQueued;

    private void OnAddressBookPushChanged(string actorId, string addressbookId) =>
        RefreshAddressBookIfShowing();

    private void OnAddressBookReconnected() => RefreshAddressBookIfShowing();

    /// <summary>Re-read the books and the open book's cards, ONLY while the Address
    /// Book segment is showing (page-gated: off-segment, the nav-in read is the
    /// backstop and a re-read would be work nobody sees).</summary>
    private async void RefreshAddressBookIfShowing()
    {
        if (AddressBookView.Visibility != Visibility.Visible) return;
        if (_bookRefreshRunning)
        {
            _bookRefreshQueued = true;
            return;
        }
        _bookRefreshRunning = true;
        try
        {
            do
            {
                _bookRefreshQueued = false;
                await LoadAddressbooksAsync();
            } while (_bookRefreshQueued && AddressBookView.Visibility == Visibility.Visible);
        }
        finally
        {
            _bookRefreshRunning = false;
        }
    }

    private void PeopleSegment_Click(object sender, RoutedEventArgs e)
    {
        PeopleView.Visibility = Visibility.Visible;
        AddressBookView.Visibility = Visibility.Collapsed;
    }

    private async void AddressBookSegment_Click(object sender, RoutedEventArgs e)
    {
        PeopleView.Visibility = Visibility.Collapsed;
        AddressBookView.Visibility = Visibility.Visible;
        if (!_addressBooksLoaded)
        {
            _addressBooksLoaded = true;
            await LoadAddressbooksAsync();
        }
    }

    private async System.Threading.Tasks.Task LoadAddressbooksAsync()
    {
        if (_rpc is null) return;
        AddressBookLoadProgress.IsActive = true;
        AddressBookLoadProgress.Visibility = Visibility.Visible;
        AddressBookErrorBar.IsOpen = false;
        try
        {
            var rows = await _rpc.CarddavListAddressbooksAsync();
            var books = rows.Select(AddressbookInfo.FromFfi).ToList();
            AddressbooksList.ItemsSource = books;
            NoAddressbooksText.Visibility = books.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
            // Keep the open book open across a re-list; else auto-open the first so the
            // card list isn't empty on entry (web/linux parity: loadAddressbooks /
            // update_book_list).
            var toOpen = _openBook.ReconcileAfterRelist(books);
            if (toOpen is not null)
            {
                await LoadCardsAsync(toOpen);
            }
            else
            {
                CardsList.ItemsSource = null;
                NoCardsText.Visibility = Visibility.Collapsed;
            }
        }
        catch (Exception ex)
        {
            AddressBookErrorBar.Message = Strings.Error(ex);
            AddressBookErrorBar.IsOpen = true;
        }
        finally
        {
            AddressBookLoadProgress.IsActive = false;
            AddressBookLoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Open a book in the picker → load its card list. A flat Button
    /// row (the EventsPage event-card idiom, foreground-independent InvokePattern
    /// — see the AddressbooksList XAML comment), not a ListView selection.</summary>
    private async void AddressbookItem_Click(object sender, RoutedEventArgs e)
    {
        if (_rpc is null || sender is not Button btn || btn.Tag is not AddressbookInfo book) return;
        await LoadCardsAsync(book.Id);
    }

    private async System.Threading.Tasks.Task LoadCardsAsync(string addressbookIdHex)
    {
        if (_rpc is null) return;
        // Record the book as open BEFORE the fetch so its reply is accepted below.
        _openBook.Open(addressbookIdHex);
        AddressBookLoadProgress.IsActive = true;
        AddressBookLoadProgress.Visibility = Visibility.Visible;
        AddressBookErrorBar.IsOpen = false;
        try
        {
            var rows = await _rpc.CarddavQueryCardsAsync(addressbookIdHex);
            // The user opened another book while this fetch was in flight: a late
            // reply for a book they have left — drop it, never paint it over theirs.
            if (!_openBook.AcceptsCards(addressbookIdHex)) return;
            var cards = rows.Select(CardInfo.FromFfi).ToList();
            CardsList.ItemsSource = cards;
            NoCardsText.Visibility = cards.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        }
        catch (Exception ex)
        {
            AddressBookErrorBar.Message = Strings.Error(ex);
            AddressBookErrorBar.IsOpen = true;
        }
        finally
        {
            AddressBookLoadProgress.IsActive = false;
            AddressBookLoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Open the card-detail pane for the tapped card (real Frame
    /// navigation — the windows master→detail convention, EventsPage →
    /// EventDetailPage). A flat Button row (foreground-independent InvokePattern
    /// — see the AddressbooksList XAML comment), not a ListView selection. The
    /// already-decoded <see cref="CardInfo"/> rides the nav args directly — no
    /// FFI round-trip on open: slice 4b is read-only, so there is no live state
    /// the detail page needs to re-fetch (unlike EventDetailPage's per-caller
    /// RSVP).</summary>
    private void CardItem_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button btn || btn.Tag is not CardInfo card) return;
        Frame.Navigate(typeof(CardDetailPage), new CardDetailNavigationArgs(card));
    }
}
