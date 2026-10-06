using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// A rejected feed verb's <c>error-message</c> copy (<c>ui/feed.md</c> § Encryption at
/// rest → <i>A reply, quote or repost of a restricted post</i>, ruling 6): the refusal is
/// recognized by the real shared <c>feed_refusal_i18n_key</c> (the native dll loads in the
/// test host) and painted as <c>feed.reference_restricted</c>; any other failure keeps its
/// own text.
/// </summary>
public class FeedVerbErrorCopyTests
{
    // `fauna_client_core::post::REFERENCE_REFUSED_RESTRICTED` — the stable text the
    // refusal crosses the FFI as. Spelled out here on purpose: if the shared text moves,
    // the shared recognizer moves with it and this conformance check says so.
    private const string RefusalText =
        "this post is restricted to a smaller audience, and a reply or quote with text "
        + "would be public — it was not sent";

    [Fact]
    public void ARefusedReference_ReadsAsTheLocalizedReason()
    {
        Assert.Equal(
            Strings.Get("feed/reference_restricted"),
            FeedViewModel.VerbErrorCopy(new FfiException.General(RefusalText)));
    }

    [Fact]
    public void AnyOtherFailure_KeepsItsOwnText()
    {
        Assert.Equal(
            "nest unreachable",
            FeedViewModel.VerbErrorCopy(new FfiException.General("nest unreachable")));
    }
}
