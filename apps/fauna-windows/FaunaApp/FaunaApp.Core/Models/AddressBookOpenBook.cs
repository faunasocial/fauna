namespace FaunaApp.Core.Models;

/// <summary>
/// Which address book the Contacts page's Address Book segment has open — the
/// bookkeeping that makes a <c>fauna.addressbook.changed</c> re-read safe
/// (transport.md § Push events): a re-list must keep the open book open rather than
/// jumping back to the first, and a card reply for a book the user has since left
/// must be dropped rather than painted over the book they are reading. The windows
/// twin of linux <c>AddressBookHandles::open_book</c> +
/// <c>update_book_list</c>/<c>update_card_list</c>, kept out of the page's
/// code-behind so the rule is unit-testable.
/// </summary>
public sealed class AddressBookOpenBook
{
    /// <summary>The hex id of the book whose cards the list shows (or was last asked
    /// to show); <c>null</c> when no book is open.</summary>
    public string? Id { get; private set; }

    /// <summary>Record <paramref name="addressbookId"/> as the open book — call
    /// BEFORE fetching its cards, so the reply is accepted and a later re-list keeps
    /// it open.</summary>
    public void Open(string addressbookId) => Id = addressbookId;

    /// <summary>Whether a card reply for <paramref name="addressbookId"/> should be
    /// painted: only the open book's. Any other is a late reply for a book the user
    /// has left.</summary>
    public bool AcceptsCards(string addressbookId) => Id == addressbookId;

    /// <summary>
    /// Settle the open book after the book list was (re-)read: the current book when
    /// it is still listed, else the first book (so the card list isn't empty on
    /// entry — web/linux parity), else none. Returns the id whose cards the caller
    /// should now fetch, or <c>null</c> when the list is empty.
    /// </summary>
    public string? ReconcileAfterRelist(IReadOnlyList<AddressbookInfo> books)
    {
        var next = Id is { } open && books.Any(b => b.Id == open)
            ? open
            : books.FirstOrDefault()?.Id;
        Id = next;
        return next;
    }
}
