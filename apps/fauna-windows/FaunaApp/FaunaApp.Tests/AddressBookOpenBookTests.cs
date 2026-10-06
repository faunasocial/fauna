using FaunaApp.Core.Models;
using Xunit;

// The Contacts page's Address Book "which book is open" bookkeeping — what makes a
// `fauna.addressbook.changed` re-read safe (transport.md § Push events): the open book
// stays open across the re-list, and a card reply for a book the user has since left is
// dropped. The windows twin of linux `AddressBookHandles::open_book` +
// `update_book_list`/`update_card_list`, kept out of the XAML code-behind so the rule is
// unit-testable (the page itself needs a running WinUI dispatcher).
public class AddressBookOpenBookTests
{
    private static AddressbookInfo Book(string id) => new(id, $"Book {id}", 0);

    [Fact]
    public void NothingIsOpenBeforeTheFirstOpen_SoNoCardReplyIsAccepted()
    {
        var open = new AddressBookOpenBook();

        Assert.Null(open.Id);
        Assert.False(open.AcceptsCards("a"));
    }

    [Fact]
    public void OpeningABookAcceptsItsCards_AndDropsEveryOthers()
    {
        var open = new AddressBookOpenBook();

        open.Open("a");

        Assert.True(open.AcceptsCards("a"));
        Assert.False(open.AcceptsCards("b"));
    }

    [Fact]
    public void ALateReplyForABookTheUserLeftIsDropped()
    {
        var open = new AddressBookOpenBook();
        open.Open("a");
        // The user taps book b while book a's card fetch is still in flight.
        open.Open("b");

        Assert.False(open.AcceptsCards("a"));
        Assert.True(open.AcceptsCards("b"));
    }

    [Fact]
    public void ARelistKeepsTheOpenBookOpen_NotJumpingBackToTheFirst()
    {
        var open = new AddressBookOpenBook();
        open.Open("b");

        var toOpen = open.ReconcileAfterRelist(new[] { Book("a"), Book("b"), Book("c") });

        Assert.Equal("b", toOpen);
        Assert.Equal("b", open.Id);
    }

    [Fact]
    public void ARelistThatNoLongerListsTheOpenBookFallsBackToTheFirst()
    {
        var open = new AddressBookOpenBook();
        open.Open("gone");

        var toOpen = open.ReconcileAfterRelist(new[] { Book("a"), Book("b") });

        Assert.Equal("a", toOpen);
        Assert.Equal("a", open.Id);
        Assert.False(open.AcceptsCards("gone"));
    }

    [Fact]
    public void TheFirstListingOpensTheFirstBook()
    {
        var open = new AddressBookOpenBook();

        var toOpen = open.ReconcileAfterRelist(new[] { Book("a"), Book("b") });

        Assert.Equal("a", toOpen);
        Assert.Equal("a", open.Id);
    }

    [Fact]
    public void AnEmptyListingClosesTheBook_AndNothingIsAcceptedAfterwards()
    {
        var open = new AddressBookOpenBook();
        open.Open("a");

        var toOpen = open.ReconcileAfterRelist(Array.Empty<AddressbookInfo>());

        Assert.Null(toOpen);
        Assert.Null(open.Id);
        Assert.False(open.AcceptsCards("a"));
    }
}
