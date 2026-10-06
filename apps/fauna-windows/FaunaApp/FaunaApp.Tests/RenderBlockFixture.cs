using uniffi.fauna_core;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint a <see cref="RenderBlock.QuotedPost"/> block with named
/// overrides for just the fields a test cares about, instead of every call site
/// hand-listing the full positional record — the sibling of
/// <see cref="FaunaApp.Core.ViewModels.PostSummaryFixture"/> for the second type that
/// recurred. No production code constructs a <c>RenderBlock.QuotedPost</c> directly (it
/// always arrives folded into a <c>PostSummary.document</c> off the FFI boundary), so
/// this stays test-only.
/// </summary>
internal static class RenderBlockFixture
{
    internal static RenderBlock QuotedPost(
        string postId,
        string author,
        string body,
        VerificationStatus verification = VerificationStatus.Unchecked,
        AuthoringOriginStatus authoringOrigin = AuthoringOriginStatus.Unknown,
        string? legalTakedownRef = null,
        bool notFound = false) =>
        new RenderBlock.QuotedPost(postId, author, body, verification, authoringOrigin, legalTakedownRef, notFound);
}
