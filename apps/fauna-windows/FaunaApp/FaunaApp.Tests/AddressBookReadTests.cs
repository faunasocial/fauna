using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Address Book segment never waits on a credential
/// (<c>docs/goal/ui/contacts.md</c> § Errors &amp; edge cases, ruled 2026-11-18): the
/// typed read's <i>unresolved</i> answer — a missing key included, the first-connect
/// mint in flight — paints the loading line, never "No address books yet."; only a
/// loaded read with no book says that (<see cref="AddressBookRead"/>, the windows twin
/// of linux's <c>book_placeholder_text</c>).
///
/// <para>Joins the <c>StringsGlobal</c> collection: every expected line is read through
/// <see cref="Strings"/> at assertion time, so the cases hold whichever localizer the
/// process carries.</para>
/// </summary>
[Collection("StringsGlobal")]
public class AddressBookReadTests
{
    [Fact]
    public void AnUnresolvedRead_PaintsLoading_NeverAnEmptyAddressBook()
    {
        var paint = AddressBookRead.Of(new FfiAddressbooksRead.Unresolved());

        Assert.False(paint.Resolved);
        Assert.Empty(paint.Books);
        Assert.Equal(Strings.Get("contacts/address_book/loading"), paint.Placeholder);
        Assert.NotEqual(Strings.Get("contacts/address_book/no_addressbooks"), paint.Placeholder);
    }

    [Fact]
    public void ALoadedReadWithNoBook_SaysNoAddressBooksYet()
    {
        var paint = AddressBookRead.Of(new FfiAddressbooksRead.Loaded(new FfiAddressbookRow[0]));

        Assert.True(paint.Resolved);
        Assert.Empty(paint.Books);
        Assert.Equal(Strings.Get("contacts/address_book/no_addressbooks"), paint.Placeholder);
    }

    [Fact]
    public void ALoadedReadWithBooks_ListsThem_WithNoPlaceholder()
    {
        var paint = AddressBookRead.Of(new FfiAddressbooksRead.Loaded(new[]
        {
            MockNestRpcClient.MakeAddressbookRow("aa", "Personal", 3),
            MockNestRpcClient.MakeAddressbookRow("bb", "Work"),
        }));

        Assert.True(paint.Resolved);
        Assert.Null(paint.Placeholder);
        Assert.Equal(new[] { "aa", "bb" }, paint.Books.Select(b => b.Id));
        Assert.Equal(3u, paint.Books[0].CardCount);
    }

    /// <summary>The page's read rides the typed seam: the mock answers whatever the
    /// test sets, unresolved by default — the state a fresh account starts in.</summary>
    [Fact]
    public async Task TheTypedSeam_AnswersUnresolved_UntilTheReadLoads()
    {
        var rpc = new MockNestRpcClient();
        Assert.IsType<FfiAddressbooksRead.Unresolved>(await rpc.CarddavReadAddressbooksAsync());

        rpc.NextAddressbooksRead = new FfiAddressbooksRead.Loaded(new[]
        {
            MockNestRpcClient.MakeAddressbookRow("aa", "Personal"),
        });
        var paint = AddressBookRead.Of(await rpc.CarddavReadAddressbooksAsync());

        Assert.True(paint.Resolved);
        Assert.Single(paint.Books);
        Assert.Contains("CarddavReadAddressbooks", rpc.Calls);
    }
}
