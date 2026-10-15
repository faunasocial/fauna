using FaunaApp.Core.Models;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The knock row's sender label and the contact row's name. Both are bound twice in
/// <c>ContactsPage.xaml</c>: as the visible text, and as the row root's
/// <c>AutomationProperties.Name</c>. A <c>Grid</c> template root with no Name is
/// pruned from UI Automation, so the row's own id (<c>knock-card</c>,
/// <c>contact-row</c>) counts 0 even though its children render. Both labels
/// therefore must never be empty. Both are the shared overlay face's answer
/// (<c>FfiContactOverlays::peer_label</c>); windows derives neither.
/// <para>
/// This is what kept <c>test_knock_live_refresh.py[windows]</c> red: the knock row
/// bound the sender to <c>Handle</c>, which <c>fauna.knocks.list</c> never
/// supplies, and <c>FallbackValue</c> does not cover a null value.
/// </para>
/// </summary>
public class KnockInfoTests
{
    private static readonly string Sender = "ab".PadRight(64, 'c');

    private static ContactsViewModel NewVm() => new(new MockNestRpcClient());

    [Fact]
    public void SenderLabel_IsTheCanonicalShortIdWhenNoNicknameIsSet()
    {
        var vm = NewVm();
        vm.Knocks.Add(new KnockInfo(Sender, "wants to connect", 1000));
        vm.RefreshNames();

        var row = Assert.Single(vm.KnockRows);
        Assert.Equal(FaunaFfiMethods.ShortId(Sender), row.SenderLabel);
        Assert.False(string.IsNullOrEmpty(row.SenderLabel));
    }

    [Fact]
    public void ContactName_IsTheHandleWhenThereIsOne()
    {
        var vm = NewVm();
        vm.Contacts.Add(new ContactInfo(Sender, "alice", ContactStatus.Accepted, null));
        vm.RefreshNames();

        var row = Assert.Single(vm.Rows);
        Assert.Equal("alice", row.Name);
        Assert.Null(row.PublicName);
        Assert.Null(row.Labels);
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    public void ContactName_FallsBackToTheCanonicalShortIdWithoutAHandle(string? handle)
    {
        // A federated peer's row carries no handle (contacts.md § State & data shape).
        var vm = NewVm();
        vm.Contacts.Add(new ContactInfo(Sender, handle, ContactStatus.Accepted, null));
        vm.RefreshNames();

        Assert.Equal(FaunaFfiMethods.ShortId(Sender), Assert.Single(vm.Rows).Name);
    }
}
