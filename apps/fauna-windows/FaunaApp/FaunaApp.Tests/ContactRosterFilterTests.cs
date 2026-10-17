using System.Linq;
using FaunaApp.Core.Models;
using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The <c>contacts-search-field</c> roster filter is single-sourced in shared Rust
/// (<c>FfiContactOverlays::matches_filter</c>, which also matches the viewer's nickname
/// and labels; contacts.md § The private overlay). These call the REAL UniFFI face
/// (native <c>fauna_ffi</c> dll loads in the test host — memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>) through
/// <see cref="ContactsViewModel.FilterRows"/>, the windows seam, over a manager with
/// no overlay saved: pinned here is the row→predicate arg-wiring, not the predicate
/// (the shared crate pins that). The windows-owned gain: it matches on <b>domain</b>
/// too, and on the enriched <c>FfiContactItem.handle</c>.
/// </summary>
public class ContactRosterFilterTests
{
    private static ContactInfo Row(string actorId, string? handle, string? domain) =>
        new(actorId, handle, ContactStatus.Accepted, null) { Domain = domain };

    private static readonly ContactInfo Alice = Row("aa".PadRight(64, 'a'), "alice", "example.com");
    private static readonly ContactInfo Bob = Row("bb".PadRight(64, 'b'), "bob", "other.org");

    private static ContactsViewModel Loaded()
    {
        var vm = new ContactsViewModel(new MockNestRpcClient());
        vm.Contacts.Add(Alice);
        vm.Contacts.Add(Bob);
        vm.RefreshNames();
        return vm;
    }

    private static string[] Matches(string query) =>
        Loaded().FilterRows(query).Select(r => r.ActorId).ToArray();

    [Fact]
    public void FilterRows_EmptyQuery_MatchesAll()
    {
        Assert.Equal(new[] { Alice.ActorId, Bob.ActorId }, Matches(""));
        Assert.Equal(new[] { Alice.ActorId, Bob.ActorId }, Matches("   "));
    }

    [Fact]
    public void FilterRows_MatchesOnHandle()
    {
        Assert.Equal(new[] { Alice.ActorId }, Matches("alic"));
        Assert.Equal(new[] { Alice.ActorId }, Matches("ALICE")); // case-insensitive
    }

    [Fact]
    public void FilterRows_MatchesOnDomain()
    {
        Assert.Equal(new[] { Alice.ActorId }, Matches("example"));
        Assert.Equal(new[] { Bob.ActorId }, Matches("other.org"));
    }

    [Fact]
    public void FilterRows_MatchesOnActorIdHex()
    {
        Assert.Equal(new[] { Bob.ActorId }, Matches("bb")); // actor-id hex prefix
    }

    [Fact]
    public void FilterRows_NoMatch_IsEmpty()
    {
        Assert.Empty(Matches("zzz"));
    }
}
