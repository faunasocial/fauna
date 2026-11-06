using System.Linq;
using FaunaApp.Core.Models;
using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The contacts roster paints one section per relationship status, in the shared
/// order (contacts.md § Where logic lives → Roster status grouping):
/// <c>fauna_core::format::contact_roster_group_order</c> through its UniFFI face
/// <c>FaunaFfiMethods.ContactRosterGroupOrder</c> — confirmed, accepted, pending,
/// blocked, then every status this build does not name in first-seen order — each
/// section headed by <c>contact_status_label</c>, its rows in the roster's own order.
/// These call the REAL face (the native dll loads in the test host): pinned here is
/// the windows wiring — the raw wire status reaching the order and the heading, so a
/// status windows' enum does not name keeps its own section instead of filing under
/// Accepted — not the order itself (the shared crate pins that).
/// </summary>
public class ContactRosterGroupingTests
{
    private static ContactInfo Row(char fill, string wire) =>
        new(fill.ToString().PadRight(64, fill), $"user{fill}", ContactStatusWire.Parse(wire), null)
        {
            WireStatus = wire,
        };

    // Nest order: an unknown status first, the canonical ones shuffled behind it.
    private static readonly ContactInfo A = Row('a', "later");
    private static readonly ContactInfo B = Row('b', "accepted");
    private static readonly ContactInfo C = Row('c', "confirmed");
    private static readonly ContactInfo D = Row('d', "accepted");
    private static readonly ContactInfo E = Row('e', "later");
    private static readonly ContactInfo F = Row('f', "pending");

    private static ContactsViewModel Loaded()
    {
        var vm = new ContactsViewModel(new MockNestRpcClient());
        foreach (var c in new[] { A, B, C, D, E, F })
            vm.Contacts.Add(c);
        vm.RefreshNames();
        return vm;
    }

    [Fact]
    public void Sections_FollowTheSharedOrder_WithTheUnknownStatusLast()
    {
        var vm = Loaded();
        Assert.Equal(
            new[] { "confirmed", "accepted", "pending", "later" },
            vm.Sections.Select(s => s.Status).ToArray());
    }

    [Fact]
    public void Sections_KeepTheRosterOrderWithinEachSection()
    {
        var vm = Loaded();
        Assert.Equal(
            new[] { C.ActorId, B.ActorId, D.ActorId, F.ActorId, A.ActorId, E.ActorId },
            vm.Sections.SelectMany(s => s).Select(r => r.ActorId).ToArray());
    }

    [Fact]
    public void Sections_AreHeadedByTheSharedStatusLabel()
    {
        // The test host has no localizer, so a known status resolves to its dotted
        // key; an unknown one is the shared label's capitalized verbatim text.
        var vm = Loaded();
        Assert.Equal(
            new[] { "common.confirmed", "common.accepted", "common.pending", "Later" },
            vm.Sections.Select(s => s.Heading).ToArray());
    }

    [Fact]
    public void FilterSections_NarrowsWithinSections_AndDropsAnEmptiedOne()
    {
        // "userc" matches only C, so the accepted, pending and unknown sections paint
        // nothing — no heading over an empty section.
        var sections = Loaded().FilterSections("userc");
        Assert.Equal(new[] { "confirmed" }, sections.Select(s => s.Status).ToArray());
        Assert.Equal(new[] { C.ActorId }, sections.Single().Select(r => r.ActorId).ToArray());
    }

    [Fact]
    public void FilterSections_NoMatch_IsEmpty()
    {
        Assert.Empty(Loaded().FilterSections("zzz"));
    }

    [Fact]
    public void RefreshNames_RebuildsTheSections()
    {
        var vm = Loaded();
        vm.Contacts.Remove(F);
        vm.RefreshNames();
        Assert.Equal(
            new[] { "confirmed", "accepted", "later" },
            vm.Sections.Select(s => s.Status).ToArray());
    }

    [Theory]
    [InlineData("pending", ContactStatus.Pending)]
    [InlineData("accepted", ContactStatus.Accepted)]
    [InlineData("confirmed", ContactStatus.Confirmed)]
    [InlineData("blocked", ContactStatus.Blocked)]
    [InlineData("later", ContactStatus.Other)]
    public void WireStatus_ParsesToTheEnum_AndAnUnnamedOneIsOther(string wire, ContactStatus expected)
    {
        Assert.Equal(expected, ContactStatusWire.Parse(wire));
    }

    [Fact]
    public void WireStatus_DefaultsToTheEnumName()
    {
        var c = new ContactInfo("a".PadRight(64, 'a'), null, ContactStatus.Confirmed, null);
        Assert.Equal("confirmed", c.WireStatus);
    }
}
