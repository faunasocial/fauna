using FaunaApp.Core.Models;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The <c>contacts-search-field</c> roster filter is single-sourced in shared Rust
/// (<c>fauna_core::format::contact_matches_filter</c> via the value-format FFI
/// wrapper <see cref="FaunaFfiMethods.ContactMatchesFilter"/>; contacts.md § Where
/// logic lives → Contact roster filter). These call the REAL UniFFI export (native
/// <c>fauna_ffi</c> dll loads in the test host — memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>) through
/// <see cref="ContactsViewModel.FilterRoster"/>, the windows seam. The headline of
/// the lift: windows now matches on <b>domain</b> too (it previously matched only
/// handle + actor-id), and on the enriched <c>FfiContactItem.handle</c> the handle
/// match actually fires (the WS-RPC map used to drop it to null).
/// </summary>
public class ContactRosterFilterTests
{
    private static ContactInfo Row(string actorId, string? handle, string? domain) =>
        new(actorId, handle, ContactStatus.Accepted, null) { Domain = domain };

    private static readonly ContactInfo Alice = Row("aa".PadRight(64, 'a'), "alice", "example.com");
    private static readonly ContactInfo Bob = Row("bb".PadRight(64, 'b'), "bob", "other.org");
    private static readonly ContactInfo[] Roster = { Alice, Bob };

    [Fact]
    public void FilterRoster_EmptyQuery_MatchesAll()
    {
        Assert.Equal(Roster, FilterRoster(""));
        Assert.Equal(Roster, FilterRoster("   "));
    }

    [Fact]
    public void FilterRoster_MatchesOnHandle()
    {
        Assert.Equal(new[] { Alice }, FilterRoster("alic"));
        Assert.Equal(new[] { Alice }, FilterRoster("ALICE")); // case-insensitive
    }

    [Fact]
    public void FilterRoster_MatchesOnDomain_TheLiftGain()
    {
        // Domain matching is the functional gain — windows previously matched only
        // handle + actor-id, so these queries returned nothing.
        Assert.Equal(new[] { Alice }, FilterRoster("example"));
        Assert.Equal(new[] { Bob }, FilterRoster("other.org"));
    }

    [Fact]
    public void FilterRoster_MatchesOnActorIdHex()
    {
        Assert.Equal(new[] { Bob }, FilterRoster("bb")); // actor-id hex prefix
    }

    [Fact]
    public void FilterRoster_NoMatch_IsEmpty()
    {
        Assert.Empty(FilterRoster("zzz"));
    }

    [Theory]
    [InlineData("", null, null, "abc", true)]            // empty query → all (even null handle/domain)
    [InlineData("ab", null, null, "ABCD", true)]         // actor-id, case-insensitive
    [InlineData("ali", "alice", null, "abc", true)]      // handle, null domain
    [InlineData("ex", null, "example.com", "abc", true)] // domain, null handle
    [InlineData("zzz", "alice", "example.com", "abc", false)]
    public void ContactMatchesFilter_RawPredicate_Conformance(
        string query, string? handle, string? domain, string actorId, bool expected)
    {
        Assert.Equal(expected, FaunaFfiMethods.ContactMatchesFilter(query, handle, domain, actorId));
    }

    private static System.Collections.Generic.IReadOnlyList<ContactInfo> FilterRoster(string query) =>
        ContactsViewModel.FilterRoster(Roster, query);
}
