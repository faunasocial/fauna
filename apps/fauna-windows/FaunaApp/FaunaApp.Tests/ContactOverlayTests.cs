using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The private contact overlay's windows glue (contacts.md § The private overlay;
/// profile.md § The private section). The names, the roster filter, the staging and
/// every refusal are shared Rust behind <c>FfiContactOverlays</c> / <c>FfiOverlayEditor</c>
/// and are pinned there; these pin what windows adds over the REAL UniFFI face (the
/// native <c>fauna_ffi</c> dll loads in the test host — memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>): the view models' reading of
/// the editor, the painted rows, and that no surface re-derives a name. The manager
/// source is left unwired, so every read is over an EMPTY projection — a person nobody
/// saved an overlay for.
/// </summary>
public class ContactOverlayTests
{
    private static readonly string Aa = "aa".PadRight(64, 'a');
    private static readonly string Bb = "bb".PadRight(64, 'b');

    // ── the private section's view model ──────────────────────────────────────

    /// <summary>Every gesture only stages: the form follows the editor, a refused label
    /// stages nothing and shows its refusal, and a removal drops the chip.</summary>
    [Fact]
    public void PrivateSection_StagesThroughTheSharedEditor()
    {
        var vm = new ProfilePrivateViewModel();
        vm.Open(Aa);
        Assert.Equal("", vm.Nickname);
        Assert.Equal("", vm.Notes);
        Assert.Empty(vm.Labels);

        vm.SetNickname("Mum");
        vm.SetNotes("likes tea");
        Assert.True(vm.AddLabel("Book club"));
        Assert.Equal("Mum", vm.Nickname);
        Assert.Equal("likes tea", vm.Notes);
        Assert.Equal(new[] { "Book club" }, vm.Labels.ToArray());
        Assert.Null(vm.ErrorMessage);

        // A blank label is refused by the shared editor; the refusal text is its own.
        Assert.False(vm.AddLabel("   "));
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        Assert.Equal(new[] { "Book club" }, vm.Labels.ToArray());

        vm.RemoveLabel(0);
        Assert.Empty(vm.Labels);
    }

    /// <summary>A Save that cannot be written keeps the staged edits on screen and puts
    /// the shared text on the page's error — here the account store has not assembled
    /// (no login in this process).</summary>
    [Fact]
    public async Task ASaveThatCannotBeWritten_KeepsTheStagedEdits()
    {
        var vm = new ProfilePrivateViewModel();
        vm.Open(Aa);
        vm.SetNickname("Mum");

        await vm.SaveAsync();

        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        Assert.Equal("Mum", vm.Nickname);
        Assert.False(vm.IsSaving);
    }

    /// <summary>A Save with nothing staged writes nothing and is not an error — the
    /// shared face answers rather than hanging (the staging-guard deadlock fixed in
    /// shared Rust, pinned there).</summary>
    [Fact]
    public async Task ASaveWithNothingStaged_IsNotAnError()
    {
        var vm = new ProfilePrivateViewModel();
        vm.Open(Aa);

        await vm.SaveAsync();

        Assert.Null(vm.ErrorMessage);
        Assert.False(vm.IsSaving);
    }

    /// <summary>Opening another person's profile starts clean: nothing staged for the
    /// last person carries over, and neither does their refusal.</summary>
    [Fact]
    public void OpeningAnotherPerson_DropsTheLastOnesStaging()
    {
        var vm = new ProfilePrivateViewModel();
        vm.Open(Aa);
        vm.SetNickname("Mum");
        vm.AddLabel("   ");
        Assert.NotNull(vm.ErrorMessage);

        vm.Open(Bb);
        Assert.Equal("", vm.Nickname);
        Assert.Null(vm.ErrorMessage);

        vm.Reset();
        vm.SetNickname("ignored");
        Assert.Equal("", vm.Nickname);
    }

    // ── the names ────────────────────────────────────────────────────────────

    /// <summary>With no overlay the roster row, the knock card and the Profile header
    /// all show the public name only — and neither secondary line.</summary>
    [Fact]
    public void WithoutAnOverlay_EverySurfaceShowsThePublicNameOnly()
    {
        var contacts = new ContactsViewModel(new MockNestRpcClient());
        contacts.Contacts.Add(new ContactInfo(Aa, "alice", ContactStatus.Confirmed, null));
        contacts.Contacts.Add(new ContactInfo(Bb, null, ContactStatus.Confirmed, null));
        contacts.Knocks.Add(new KnockInfo(Bb, null, 1));
        contacts.RefreshNames();

        Assert.Equal(new[] { "alice", FaunaFfiMethods.ShortId(Bb) }, contacts.Rows.Select(r => r.Name).ToArray());
        Assert.All(contacts.Rows, r => { Assert.Null(r.PublicName); Assert.Null(r.Labels); });
        Assert.Equal(FaunaFfiMethods.ShortId(Bb), Assert.Single(contacts.KnockRows).SenderLabel);
    }

    /// <summary>Reading names never moves the revision: the face is built afresh on
    /// every read, and a read that re-rendered its own reader would loop.</summary>
    [Fact]
    public void ReadingTheFace_DoesNotMoveTheRevision()
    {
        ulong first, second;
        using (var names = ContactOverlayNames.Open())
        {
            first = names.Revision();
            _ = names.PeerLabel(null, "alice", Aa);
            _ = names.LabelsLine(Aa);
        }
        using (var names = ContactOverlayNames.Open()) second = names.Revision();
        Assert.Equal(first, second);

        // …and so the page's re-paint guard sees nothing to do.
        var vm = new ContactsViewModel(new MockNestRpcClient());
        vm.RefreshNames();
        Assert.False(vm.RefreshNamesIfMoved());
    }

    /// <summary>The subscription row's creator label is the face's answer: the handle the
    /// nest resolved, else the hex actor id.</summary>
    [Fact]
    public void SubscriptionAuthor_IsTheFacesAnswer()
    {
        var actor = new byte[32];
        using var names = ContactOverlayNames.Open();
        Assert.Equal("alice", names.SubscriptionAuthorLabel("alice", actor));
        Assert.Equal(FaunaFfiMethods.HexFull(actor), names.SubscriptionAuthorLabel(null, actor));
    }

    /// <summary>The feed card's author is the face's answer over the post's own
    /// author-display: the bridged name, else the canonical short id.</summary>
    [Fact]
    public void FeedAuthor_IsTheFacesAnswer()
    {
        var anonymous = new FeedPostItem(PostSummaryFixture.Make(postId: "p", author: Aa));
        Assert.Equal(FaunaFfiMethods.ShortId(Aa), anonymous.AuthorLabel);

        var bridged = new FeedPostItem(PostSummaryFixture.Make(
            postId: "p", author: Aa,
            authorDisplay: new uniffi.fauna_feed.AuthorDisplayView("a@b.c", "Alice", null)));
        Assert.Equal("Alice", bridged.AuthorLabel);
        Assert.False(anonymous.ContentEquals(bridged), "a renamed author rebuilds the card");
    }
}
