using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Unit tests for the computed display properties on AttendeeInfo.
/// Each property is view-glue over the raw CalDAV attendee fields — behaviour
/// must exactly match android AttendeeRow and web statusColor (events.md
/// § Attendee list presentation, 2026-06-23).
/// </summary>
public class AttendeeInfoTests
{
    // --- Monogram ---

    [Fact]
    public void Monogram_EmailOnlyAttendee_ReturnsEmailInitial()
    {
        var a = new AttendeeInfo("alice@example.com", "", "going");
        Assert.Equal("A", a.Monogram);
    }

    [Fact]
    public void Monogram_NamePresent_ReturnsFirstCharUppercase()
    {
        var a = new AttendeeInfo("bob@example.com", "bob smith", "going");
        Assert.Equal("B", a.Monogram);
    }

    [Fact]
    public void Monogram_NameAlreadyUppercase_ReturnsItUnchanged()
    {
        var a = new AttendeeInfo("c@x.test", "Carol", "declined");
        Assert.Equal("C", a.Monogram);
    }

    // --- DisplayName ---

    [Fact]
    public void DisplayName_NoName_ReturnsEmail()
    {
        var a = new AttendeeInfo("d@x.test", "", "going");
        Assert.Equal("d@x.test", a.DisplayName);
    }

    [Fact]
    public void DisplayName_NameEqualsEmail_ReturnsEmail()
    {
        var a = new AttendeeInfo("e@x.test", "e@x.test", "going");
        Assert.Equal("e@x.test", a.DisplayName);
    }

    [Fact]
    public void DisplayName_DistinctName_ReturnsName()
    {
        var a = new AttendeeInfo("f@x.test", "Frank", "interested");
        Assert.Equal("Frank", a.DisplayName);
    }

    // --- EmailLine ---

    [Fact]
    public void EmailLine_NoName_IsNull()
    {
        var a = new AttendeeInfo("g@x.test", "", "going");
        Assert.Null(a.EmailLine);
    }

    [Fact]
    public void EmailLine_NameEqualsEmail_IsNull()
    {
        var a = new AttendeeInfo("h@x.test", "h@x.test", "going");
        Assert.Null(a.EmailLine);
    }

    [Fact]
    public void EmailLine_DistinctName_ReturnsEmail()
    {
        var a = new AttendeeInfo("i@x.test", "Ivan", "declined");
        Assert.Equal("i@x.test", a.EmailLine);
    }

    // --- RsvpHexColor ---

    [Theory]
    [InlineData("going", "#16A34A")]
    [InlineData("accepted", "#16A34A")]
    [InlineData("interested", "#CA8A04")]
    [InlineData("tentative", "#6B7280")]
    [InlineData("declined", "#DC2626")]
    [InlineData("waitlisted", "#EA580C")]
    [InlineData("invited", "#6B7280")]
    [InlineData("unknown", "#6B7280")]
    [InlineData("", "#6B7280")]
    public void RsvpHexColor_CanonicalMap(string rsvp, string expectedHex)
    {
        var a = new AttendeeInfo("j@x.test", "J", rsvp);
        Assert.Equal(expectedHex, a.RsvpHexColor);
    }

    [Fact]
    public void RsvpHexColor_CaseInsensitive()
    {
        var a = new AttendeeInfo("k@x.test", "K", "GOING");
        Assert.Equal("#16A34A", a.RsvpHexColor);
    }

    // --- RsvpLabel (shared fauna_core::ical::rsvp_status_label via FFI) ---
    // The test host has no localizer, so Strings.Resolve falls back to the dotted
    // i18n key — the same missing-key fallback the reminder/mail-settings lifts use.

    [Theory]
    [InlineData("going", "events.rsvp.going")]
    [InlineData("interested", "events.rsvp.interested")]
    [InlineData("tentative", "events.rsvp.tentative")]
    [InlineData("declined", "events.rsvp.declined")]
    [InlineData("waitlisted", "events.rsvp.waitlisted")]
    [InlineData("invited", "events.rsvp.invited")]
    public void RsvpLabel_PresetStatus_ResolvesSharedKey(string rsvp, string expectedKey)
    {
        var a = new AttendeeInfo("j@x.test", "J", rsvp);
        Assert.Equal(expectedKey, a.RsvpLabel);
    }

    [Fact]
    public void RsvpLabel_UnknownStatus_CapitalizesVerbatim()
    {
        // Shared fn maps an unrecognized status to a capitalized verbatim key
        // (no events.rsvp.* prefix), so Resolve surfaces it as-is.
        var a = new AttendeeInfo("m@x.test", "M", "rescinded");
        Assert.Equal("Rescinded", a.RsvpLabel);
    }

    [Fact]
    public void RsvpLabel_EmptyStatus_IsEmpty()
    {
        var a = new AttendeeInfo("n@x.test", "N", "");
        Assert.Equal(string.Empty, a.RsvpLabel);
    }
}
