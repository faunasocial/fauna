using Xunit;
using FaunaApp.Core.Models;

namespace FaunaApp.Tests;

/// <summary>
/// <see cref="CardInfo.FromFfi"/> / <see cref="AddressbookInfo.FromFfi"/> map the
/// UniFFI <c>FfiCardRow</c> / <c>FfiAddressbookRow</c> reply records → the
/// Address Book segment's display records (Contacts page, slice 4b;
/// contacts.md § Address Book segment; carddav-server.md § Independent
/// enablement). The ORG-join is the one non-trivial piece of logic (everything
/// else is a straight field copy) — byte-identical to the linux/web render
/// (<c>carddav_backend::vcard_row</c>: filter empty components, join with
/// " · "), so it's covered here rather than only exercised end-to-end.
/// </summary>
public class AddressBookModelsTests
{
    [Fact]
    public void CardInfo_JoinsNonEmptyOrgComponentsWithMiddleDot()
    {
        var row = MockNestRpcClient.MakeCardRow(
            "aa11", "Ada Lovelace",
            org: new[] { "Analytical Engine", "Research" });

        var info = CardInfo.FromFfi(row);

        Assert.Equal("Analytical Engine · Research", info.Org);
    }

    [Fact]
    public void CardInfo_SkipsEmptyOrgComponents()
    {
        // A vCard ORG like "Acme;;Widgets" has an empty middle component — the
        // join must drop it, not render a stray " · · " (mirrors linux
        // vcard_maps_to_row_with_all_fields / the shared crate's org.filter).
        var row = MockNestRpcClient.MakeCardRow(
            "aa12", "Bare Org",
            org: new[] { "Acme", "", "Widgets" });

        var info = CardInfo.FromFfi(row);

        Assert.Equal("Acme · Widgets", info.Org);
    }

    [Fact]
    public void CardInfo_EmptyOrgYieldsEmptyString()
    {
        var row = MockNestRpcClient.MakeCardRow("aa13", "No Org");

        var info = CardInfo.FromFfi(row);

        Assert.Equal("", info.Org);
    }

    [Fact]
    public void CardInfo_FlattensEmailsTelsAndPreFormattedAddresses()
    {
        var row = MockNestRpcClient.MakeCardRow(
            "aa14", "Full Card",
            emails: new[] { "ada@example.com" },
            tels: new[] { "+15550142" },
            addresses: new[] { "12 Baker St, London, NW1, UK" },
            title: "Programmer", note: "first programmer");

        var info = CardInfo.FromFfi(row);

        Assert.Equal("aa14", info.Id);
        Assert.Equal("Full Card", info.FormattedName);
        Assert.Equal(new[] { "ada@example.com" }, info.Emails);
        Assert.Equal(new[] { "+15550142" }, info.Tels);
        // FfiPostalAddress.Formatted is already the pre-joined one-line render
        // (the shared crate's job, not this mapping's) — passed through verbatim.
        Assert.Equal(new[] { "12 Baker St, London, NW1, UK" }, info.Addresses);
        Assert.Equal("Programmer", info.Title);
        Assert.Equal("first programmer", info.Note);
    }

    [Fact]
    public void AddressbookInfo_MapsIdNameAndCardCount()
    {
        var row = MockNestRpcClient.MakeAddressbookRow("bb01", "Contacts", 3);

        var info = AddressbookInfo.FromFfi(row);

        Assert.Equal("bb01", info.Id);
        Assert.Equal("Contacts", info.Name);
        Assert.Equal(3u, info.CardCount);
    }
}
