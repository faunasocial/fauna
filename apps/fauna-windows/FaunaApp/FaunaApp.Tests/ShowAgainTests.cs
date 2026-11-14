using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using Xunit;
using ContentLabelEntry = uniffi.fauna_core.ContentLabelEntry;

namespace FaunaApp.Tests;

/// <summary>
/// The reporter's show-again on windows (moderation.md § Corollary — the reporter's
/// hide is theirs to undo): <c>content-policy-show-again-button</c> beside a reported
/// placeholder, naming the scope, and one press un-hiding every hidden id that
/// matched the item.
///
/// <para>The fold is shared Rust, called for real (<c>reported_show_again</c> —
/// reference_windows_dotnet_test_loads_native_ffi); these pin the C# glue: the
/// empty-list short-circuit, the keys the feed card asks with, the press's one
/// un-hide per id with the stored list back into the render state, where a failure
/// lands, and the change signal that repaints an open page. No localizer is set in
/// the test host, so a label resolves to its raw key.</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class ShowAgainTests : IDisposable
{
    public ShowAgainTests() => ContentPolicyCache.Reset();
    public void Dispose() => ContentPolicyCache.Reset();

    private const string Cid = "aa11";
    private const string Author = "bb22";
    private const string ItemLabel = "moderation.report.show_again_item";
    private const string AccountLabel = "moderation.report.show_again_account";

    // ── The fold: which ids, which label ─────────────────────────────────

    [Fact]
    public void Fold_AHiddenPostOffersToShowTheItemAgain()
    {
        ContentPolicyCache.SetHiddenContent(new[] { Cid });

        var offer = ContentPolicyCache.ShowAgainFor(Cid, Author);

        Assert.NotNull(offer);
        Assert.Equal(new[] { Cid }, offer!.Ids);
        Assert.Equal(ItemLabel, offer.Label);
    }

    [Fact]
    public void Fold_AHiddenAccountOffersToShowTheAccountAgain()
    {
        ContentPolicyCache.SetHiddenContent(new[] { Author });

        var offer = ContentPolicyCache.ShowAgainFor("any-post", Author);

        Assert.NotNull(offer);
        Assert.Equal(new[] { Author }, offer!.Ids);
        Assert.Equal(AccountLabel, offer.Label);
    }

    [Fact]
    public void Fold_BothHiddenAreUndoneByOnePress()
    {
        ContentPolicyCache.SetHiddenContent(new[] { Cid, Author });

        var offer = ContentPolicyCache.ShowAgainFor(Cid, Author);

        Assert.NotNull(offer);
        Assert.Equal(new[] { Cid, Author }, offer!.Ids);
        Assert.Equal(AccountLabel, offer.Label);
    }

    [Fact]
    public void Fold_NothingOfTheViewersIsHiddenOffersNothing()
    {
        Assert.Null(ContentPolicyCache.ShowAgainFor(Cid, Author));

        ContentPolicyCache.SetHiddenContent(new[] { "cc33" });
        Assert.Null(ContentPolicyCache.ShowAgainFor(Cid, Author));
        // A message with no plane ref and no Fauna sender can be neither hidden nor
        // shown again.
        Assert.Null(ContentPolicyCache.ShowAgainFor(null, null));
    }

    [Fact]
    public void Offer_ComparesByItsIdsAndLabel()
    {
        Assert.Equal(new ShowAgainOffer(new[] { Cid }, ItemLabel), new ShowAgainOffer(new[] { Cid }, ItemLabel));
        Assert.NotEqual(new ShowAgainOffer(new[] { Cid }, ItemLabel), new ShowAgainOffer(new[] { Cid, Author }, AccountLabel));
    }

    // ── The press ─────────────────────────────────────────────────────────

    [Fact]
    public async Task Press_UnhidesEveryMatchedIdAndStoresTheList()
    {
        var rpc = new MockNestRpcClient();
        rpc.HiddenContent.AddRange(new[] { Cid, Author, "cc33" });
        ContentPolicyCache.SetHiddenContent(rpc.HiddenContent.ToArray());
        var offer = ContentPolicyCache.ShowAgainFor(Cid, Author)!;

        var error = await ReportSheetViewModel.ShowAgainAsync(rpc, offer.Ids);

        Assert.Null(error);
        // One un-hide per matched id — and nothing else: the report is never withdrawn.
        Assert.Equal(new[] { "UnhideReported", "UnhideReported" }, rpc.Calls.ToArray());
        Assert.Equal(new[] { "cc33" }, rpc.HiddenContent);
        // The stored list reached the render state: the item paints again.
        Assert.True(ContentPolicyCache.HiddenContentEquals(new[] { "cc33" }));
        Assert.False(ContentPolicyCache.ItemVerdictFor(Array.Empty<ContentLabelEntry>(), Cid, Author).Reported);
        Assert.Null(ContentPolicyCache.ShowAgainFor(Cid, Author));
    }

    [Fact]
    public async Task Press_AFailureLandsOnTheErrorLineAndKeepsTheHide()
    {
        var rpc = new MockNestRpcClient { UnhideReportedError = new InvalidOperationException("no store") };
        rpc.HiddenContent.Add(Cid);
        ContentPolicyCache.SetHiddenContent(new[] { Cid });

        var error = await ReportSheetViewModel.ShowAgainAsync(rpc, new[] { Cid });

        Assert.StartsWith("show again:", error);
        Assert.True(ContentPolicyCache.ItemVerdictFor(Array.Empty<ContentLabelEntry>(), Cid, Author).Reported);
    }

    [Fact]
    public async Task Press_APartialFailureStillStoresWhatLanded()
    {
        // The first un-hide landed at the nest; the second failed. The render state
        // takes the list the first one returned, so what is shown is what is stored.
        var rpc = new MockNestRpcClient { UnhideReportedFailOnCall = 2 };
        rpc.HiddenContent.AddRange(new[] { Cid, Author });
        ContentPolicyCache.SetHiddenContent(new[] { Cid, Author });

        var error = await ReportSheetViewModel.ShowAgainAsync(rpc, new[] { Cid, Author });

        Assert.StartsWith("show again:", error);
        Assert.True(ContentPolicyCache.HiddenContentEquals(new[] { Author }));
    }

    [Fact]
    public async Task Press_NoIdsIsANoOp()
    {
        var rpc = new MockNestRpcClient();

        Assert.Null(await ReportSheetViewModel.ShowAgainAsync(rpc, Array.Empty<string>()));
        Assert.Empty(rpc.Calls);
    }

    // ── The repaint signal ───────────────────────────────────────────────

    [Fact]
    public void HiddenContentChanged_RaisesOnlyWhenTheListMoved()
    {
        var raised = 0;
        void OnChanged() => raised++;
        ContentPolicyCache.HiddenContentChanged += OnChanged;
        try
        {
            ContentPolicyCache.SetHiddenContent(new[] { Cid });
            Assert.Equal(1, raised);
            ContentPolicyCache.SetHiddenContent(new[] { Cid });
            Assert.Equal(1, raised);
            ContentPolicyCache.SetHiddenContent(new[] { Cid, Author });
            Assert.Equal(2, raised);
            // An empty list and no list are the same answer.
            ContentPolicyCache.SetHiddenContent(Array.Empty<string>());
            Assert.Equal(3, raised);
            ContentPolicyCache.SetHiddenContent(null);
            Assert.Equal(3, raised);
        }
        finally
        {
            ContentPolicyCache.HiddenContentChanged -= OnChanged;
        }
    }

    [Fact]
    public async Task HiddenContentChanged_RaisesWhenTheStoreChangeReReadMovesTheList()
    {
        // The store-change notice's app-wide arm (MainPage.OnRpcStoreChanged): a
        // show-again on another device of the account reaches a page already open.
        var rpc = new MockNestRpcClient();
        rpc.HiddenContent.Add(Cid);
        ContentPolicyCache.SetHiddenContent(new[] { Cid });
        var raised = 0;
        void OnChanged() => raised++;
        ContentPolicyCache.HiddenContentChanged += OnChanged;
        try
        {
            rpc.HiddenContent.Clear();
            await new ContentPolicyPreloader(rpc).RefreshHiddenContentAsync();
            Assert.Equal(1, raised);
            await new ContentPolicyPreloader(rpc).RefreshHiddenContentAsync();
            Assert.Equal(1, raised);
        }
        finally
        {
            ContentPolicyCache.HiddenContentChanged -= OnChanged;
        }
    }

    // ── The feed card ────────────────────────────────────────────────────

    [Fact]
    public void Card_AReportedPostPaintsTheButtonBesideItsPlaceholder()
    {
        ContentPolicyCache.SetHiddenContent(new[] { Cid });

        var card = new FeedPostItem(PostSummaryFixture.Make(postId: Cid, author: Author, body: "text"));

        Assert.True(card.Reported);
        Assert.True(card.ShowContentBlockedNotice);
        Assert.True(card.ShowShowAgain);
        Assert.Equal(ItemLabel, card.ShowAgainLabel);
        Assert.Equal(new[] { Cid }, card.ShowAgain!.Ids);
    }

    [Fact]
    public void Card_APostNobodyHidPaintsNoButton()
    {
        ContentPolicyCache.SetHiddenContent(new[] { "cc33" });

        var card = new FeedPostItem(PostSummaryFixture.Make(postId: Cid, author: Author, body: "text"));

        Assert.False(card.Reported);
        Assert.False(card.ShowShowAgain);
        Assert.Null(card.ShowAgain);
    }

    [Fact]
    public void Card_TheOfferIsRenderBoundContent()
    {
        // The account hidden on top of the post: same verdict, same placeholder words,
        // but the button now names the account — the row must rebuild.
        ContentPolicyCache.SetHiddenContent(new[] { Cid });
        var before = new FeedPostItem(PostSummaryFixture.Make(postId: Cid, author: Author, body: "text"));
        ContentPolicyCache.SetHiddenContent(new[] { Cid, Author });
        var after = new FeedPostItem(PostSummaryFixture.Make(postId: Cid, author: Author, body: "text"));

        Assert.Equal(before.ContentVerdict, after.ContentVerdict);
        Assert.Equal(before.Reported, after.Reported);
        Assert.False(before.ContentEquals(after));
    }
}
