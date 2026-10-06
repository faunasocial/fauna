using FaunaApp.Core.Models;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The knock row's sender label, and the contact row's display label. Both are
/// bound twice in <c>ContactsPage.xaml</c>: as the visible text, and as the row
/// root's <c>AutomationProperties.Name</c>. A <c>Grid</c> template root with no
/// Name is pruned from UI Automation, so the row's own id (<c>knock-card</c>,
/// <c>contact-row</c>) counts 0 even though its children render. Both labels
/// therefore must never be empty.
/// <para>
/// This is what kept <c>test_knock_live_refresh.py[windows]</c> red: the knock row
/// bound the sender to <c>Handle</c>, which <c>fauna.knocks.list</c> never
/// supplies, and <c>FallbackValue</c> does not cover a null value.
/// </para>
/// </summary>
public class KnockInfoTests
{
    private static readonly string Sender = "ab".PadRight(64, 'c');

    [Fact]
    public void SenderLabel_IsTheSharedShortIdOfTheSender()
    {
        var k = new KnockInfo(Sender, "wants to connect", 1000);
        Assert.Equal(FaunaFfiMethods.ShortId(Sender), k.SenderLabel);
        Assert.False(string.IsNullOrEmpty(k.SenderLabel));
    }

    [Fact]
    public void ContactDisplayLabel_IsTheHandleWhenThereIsOne()
    {
        var c = new ContactInfo(Sender, "alice", ContactStatus.Accepted, null);
        Assert.Equal("alice", c.DisplayLabel);
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    public void ContactDisplayLabel_FallsBackToTheShortIdWithoutAHandle(string? handle)
    {
        // A federated peer's row carries no handle (contacts.md § State & data shape).
        var c = new ContactInfo(Sender, handle, ContactStatus.Accepted, null);
        Assert.Equal(FaunaFfiMethods.ShortId(Sender), c.DisplayLabel);
    }
}
