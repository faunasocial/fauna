using System.Linq;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Round-trips an injected quoting post through the REAL native <c>fauna_ffi</c> dll
/// (memory <c>reference_windows_dotnet_test_loads_native_ffi</c>) to prove the
/// <c>RenderBlock.QuotedPost</c> block the shared <c>build_post_document</c> fold
/// appends to a quoting post's <c>document</c> survives the UniFFI boundary into C#
/// and is matched by <see cref="DocumentRenderer.QuotedPost"/> /
/// <see cref="FeedPostItem.HasQuotedPost"/> — the FFI-consumption half of the
/// Slice-2b quoted-embed badge (<c>security.md</c> § Client display of unverified
/// content).
///
/// <para>Distinct from <see cref="FeedPostItemTests"/>, whose quoted assertions use
/// <b>hand-built</b> C# <c>RenderDocument</c>s; this exercises the FFI-transferred
/// path the windows e2e (<c>test_feed_unverified_source.py</c>) actually hits, via
/// the same <c>inject_posts_for_test</c> seam + <c>TestPostSpec</c> JSON the
/// cross-app <c>feed_inject_posts</c> command carries. Pins Bug
/// 2: the quoted-post embed not rendering on windows is NOT an FFI/OfType failure
/// (this test stays green); the e2e count(0) is the UIA DataTemplate prune of the
/// nameless <c>quoted-post</c> Border (memory <c>reference_winui_flaui_datatemplate_name</c>).</para>
/// </summary>
public class FeedDocumentFfiTests
{
    // The exact cross-app `feed_inject_posts` payload shape (a `TestPostSpec`
    // JSON array): a focal-`Unchecked` post quoting a `Failed` post, and one
    // quoting a `Verified` post. `build_post_document` folds a `RenderBlock.QuotedPost`
    // into each quoting post's document, carrying the quoted post's verification.
    private const string QuotingJson = """
    [
      {"post_id":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
       "author":"4444444444444444444444444444444444444444444444444444444444444444",
       "body":"quoting an unverified post","verification":"Unchecked",
       "quoted":{"post_id":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                 "author":"5555555555555555555555555555555555555555555555555555555555555555",
                 "body":"the unverified quoted body","verification":"Failed"}},
      {"post_id":"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
       "author":"6666666666666666666666666666666666666666666666666666666666666666",
       "body":"quoting a verified post","verification":"Unchecked",
       "quoted":{"post_id":"0000000000000000000000000000000000000000000000000000000000000000",
                 "author":"7777777777777777777777777777777777777777777777777777777777777777",
                 "body":"the verified quoted body","verification":"Verified"}}
    ]
    """;

    /// <summary>Build a live <see cref="FfiFeedManager"/> over a non-connecting
    /// <c>NestClient</c> (its <c>new</c> opens no socket) and inject the post list —
    /// the snapshot is replaced with no nest round-trip. The 32-byte actor secret is
    /// only used to sign on <c>submit_post</c>, never on inject.</summary>
    private static FfiFeedManager SeedManager(string json)
    {
        var secret = Enumerable.Repeat((byte)7, 32).ToArray();
        var nest = new FfiNestClient("wss://127.0.0.1:0/ws", secret);
        var mgr = nest.FeedManager(secret);
        mgr.InjectPostsForTest(json);
        return mgr;
    }

    [Fact]
    public void InjectedQuotingPost_DocumentCarriesQuotedPostBlock_AcrossFfi()
    {
        var posts = SeedManager(QuotingJson).Snapshot().posts;
        Assert.Equal(2, posts.Count());

        // Post 0 quotes a Failed post; post 1 quotes a Verified post. The FFI-lifted
        // document must carry the folded RenderBlock.QuotedPost, projected back out by the
        // shared render_document_quoted_post face as a QuotedPostEmbedOwned.
        var q0 = DocumentRenderer.QuotedPost(posts[0].document);
        Assert.NotNull(q0);
        Assert.Equal("the unverified quoted body", q0!.body);
        Assert.Equal(VerificationStatus.Failed, q0.verification);

        var q1 = DocumentRenderer.QuotedPost(posts[1].document);
        Assert.NotNull(q1);
        Assert.Equal("the verified quoted body", q1!.body);
        Assert.Equal(VerificationStatus.Verified, q1.verification);
    }

    [Fact]
    public void InjectedQuotingPost_FeedPostItemDerivesQuoteCard_AcrossFfi()
    {
        var posts = SeedManager(QuotingJson).Snapshot().posts;

        // The exact projection the XAML quoted-post Border binds to (HasQuotedPost /
        // QuotedPostBody / QuotedPostIsUnverified). If these are correct off the
        // FFI-transferred PostSummary, a count(0) in the windows e2e is a UIA prune,
        // not a data failure.
        var failedQuote = new FeedPostItem(posts[0]);
        Assert.True(failedQuote.HasQuotedPost);
        Assert.Equal("the unverified quoted body", failedQuote.QuotedPostBody);
        Assert.True(failedQuote.QuotedPostIsUnverified);
        Assert.False(failedQuote.IsUnverifiedSource); // the quoting post itself is Unchecked

        var verifiedQuote = new FeedPostItem(posts[1]);
        Assert.True(verifiedQuote.HasQuotedPost);
        Assert.False(verifiedQuote.QuotedPostIsUnverified);
    }
}
