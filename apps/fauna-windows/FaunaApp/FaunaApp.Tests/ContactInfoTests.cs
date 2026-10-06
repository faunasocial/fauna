using FaunaApp.Core.Models;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Unit tests for the computed <see cref="ContactInfo.StatusLabel"/> badge text,
/// which is single-sourced in shared Rust (fauna_core::format::contact_status_label
/// via the value-format FFI wrapper). The test host has no localizer, so
/// Strings.Resolve falls back to the dotted i18n key — the same missing-key
/// fallback the reminder/mail-settings/rsvp lifts use. The key point of the lift:
/// the Confirmed status now resolves (the old per-app switch lacked a Confirmed
/// arm and rendered "Unknown").
/// </summary>
public class ContactInfoTests
{
    [Theory]
    [InlineData(ContactStatus.Pending, "common.pending")]
    [InlineData(ContactStatus.Accepted, "common.accepted")]
    [InlineData(ContactStatus.Confirmed, "common.confirmed")]
    [InlineData(ContactStatus.Blocked, "common.blocked")]
    public void StatusLabel_ResolvesSharedKey(ContactStatus status, string expectedKey)
    {
        var c = new ContactInfo("a".PadRight(64, 'a'), null, status, null);
        Assert.Equal(expectedKey, c.StatusLabel);
    }

    [Fact]
    public void StatusLabel_Confirmed_IsDistinctFromAccepted()
    {
        // Regression guard: Confirmed must not collapse to Accepted's label.
        var confirmed = new ContactInfo("b".PadRight(64, 'b'), null, ContactStatus.Confirmed, null);
        var accepted = new ContactInfo("c".PadRight(64, 'c'), null, ContactStatus.Accepted, null);
        Assert.NotEqual(accepted.StatusLabel, confirmed.StatusLabel);
    }
}
