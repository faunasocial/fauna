using System;
using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Models;

/// <summary>
/// What the Address Book segment's book picker paints from one typed address-book
/// read (<c>FfiCarddavClient.ReadAddressbooks</c> → <c>FfiAddressbooksRead</c>;
/// <c>docs/goal/ui/contacts.md</c> § Errors &amp; edge cases, the Address Book bullet).
///
/// <para>A read that has not answered — the custody read failed, or custody holds no
/// key yet because the first-connect mint is in flight (<c>mail-credentials.md</c>
/// § MSEK lifecycle → <i>Every account holds the key</i>, ruling 7) — is
/// <i>unresolved</i>: the picker lists no book and says
/// <c>contacts.address_book.loading</c>, never an empty address book and never a
/// "set up a credential" step. Only a loaded read with no book says
/// <c>contacts.address_book.no_addressbooks</c>. The windows twin of linux's
/// <c>book_placeholder_text</c> and tui's <c>address_book::render</c>.</para>
///
/// <para>Lives in <c>FaunaApp.Core</c> so a unit test pins the reading; the page
/// (<c>ContactsPage</c>) applies it, and re-runs an unresolved read on the shared
/// store-change notice.</para>
/// </summary>
public sealed record AddressBookRead(bool Resolved, IReadOnlyList<AddressbookInfo> Books, string? Placeholder)
{
    /// <summary>The picker's paint for <paramref name="read"/>. <c>internal</c>
    /// because the parameter is a UniFFI-<c>internal</c> type.</summary>
    internal static AddressBookRead Of(FfiAddressbooksRead read)
    {
        if (read is not FfiAddressbooksRead.Loaded loaded)
        {
            return new AddressBookRead(
                Resolved: false,
                Books: Array.Empty<AddressbookInfo>(),
                Placeholder: Strings.Get("contacts/address_book/loading"));
        }
        var books = loaded.@addressbooks.Select(AddressbookInfo.FromFfi).ToList();
        return new AddressBookRead(
            Resolved: true,
            Books: books,
            Placeholder: books.Count == 0 ? Strings.Get("contacts/address_book/no_addressbooks") : null);
    }
}
