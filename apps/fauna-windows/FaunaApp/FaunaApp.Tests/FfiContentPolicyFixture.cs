using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint an <see cref="FfiContentPolicy"/> with named overrides
/// for just the label(s) a test cares about, instead of every call site hand-listing
/// all four positionally — the sibling of <see cref="PostSummaryFixture"/> for the
/// pattern. Widening
/// <see cref="FfiContentPolicy"/> now touches exactly this one file.
/// </summary>
internal static class FfiContentPolicyFixture
{
    internal static FfiContentPolicy Make(
        string nsfw = "inherit",
        string spam = "inherit",
        string phishing = "inherit",
        string commercial = "inherit") =>
        new FfiContentPolicy(nsfw, spam, phishing, commercial);
}
