using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Models;

namespace FaunaApp.Views;

/// <summary>
/// Real Frame-navigation args for <see cref="CardDetailPage"/> (the windows
/// master→detail convention, mirroring <c>EventDetailNavigationArgs</c>). The
/// already-decoded <see cref="CardInfo"/> rides directly — slice 4b is
/// read-only, so there is no live per-caller state (unlike
/// <c>EventDetailNavigationArgs.EventId</c>'s RSVP re-fetch) that would justify
/// a second FFI round-trip on open; the card the user tapped in the list is
/// exactly what the detail pane renders.
/// </summary>
internal record CardDetailNavigationArgs(CardInfo Card);

/// <summary>
/// Read-only vCard detail pane (Contacts → Address Book segment, card_detail
/// sub_page; carddav-server.md § Independent enablement, contacts.md § Address
/// Book segment — slice 4b). Mirrors <see cref="EventDetailPage"/>'s
/// back-button + header + field-rows structure.
/// </summary>
public sealed partial class CardDetailPage : Page
{
    private CardInfo? _card;

    public CardDetailPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is CardDetailNavigationArgs args)
        {
            _card = args.Card;
        }
    }

    private void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_card is null) return;

        FnText.Text = _card.FormattedName;

        if (!string.IsNullOrEmpty(_card.Title))
        {
            TitleText.Text = _card.Title;
            TitleText.Visibility = Visibility.Visible;
        }
        if (!string.IsNullOrEmpty(_card.Org))
        {
            OrgText.Text = _card.Org;
            OrgText.Visibility = Visibility.Visible;
        }

        EmailsList.ItemsSource = _card.Emails;
        TelsList.ItemsSource = _card.Tels;
        AddressesList.ItemsSource = _card.Addresses;

        if (!string.IsNullOrEmpty(_card.Note))
        {
            NoteText.Text = _card.Note;
            NoteText.Visibility = Visibility.Visible;
        }
    }

    private void BackButton_Click(object sender, RoutedEventArgs e)
    {
        if (Frame.CanGoBack) Frame.GoBack();
    }
}
