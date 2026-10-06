using FaunaApp.Core.Helpers;
using uniffi.fauna_core;
using uniffi.fauna_media_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The share-link row projection and label seams (share-links.md § Flows → List,
/// § Expiry). The XAML code-behind is unreachable from this assembly, so the rules the
/// list's controls hang on live in <see cref="ShareLinkFormat"/> and are pinned here:
/// copy only where the URL re-derived, revoke only on an Active row, labels through
/// the shared maps with the raw value as the fallback.
/// </summary>
public class ShareLinkFormatTests
{
    private static ShareLinkSummary Summary(string state, string? url) =>
        new("ab12", "holiday.txt", 1_760_000_000L, state, url);

    [Fact]
    public void ActiveRowWithAVerifiedUrl_OffersCopyAndRevoke()
    {
        var row = ShareLinkFormat.MapRow(Summary("active", "https://nest/share/t"));

        Assert.Equal("ab12", row.TokenId);
        Assert.Equal("holiday.txt", row.Name);
        Assert.Equal("active", row.State);
        Assert.True(row.CanCopy);
        Assert.True(row.CanRevoke);
    }

    [Fact]
    public void ActiveRowWhoseUrlDidNotReDerive_HasNoCopy_ButKeepsRevoke()
    {
        // A token a future client minted with fields this one does not know: the copy
        // control is ABSENT rather than producing a wrong link.
        var row = ShareLinkFormat.MapRow(Summary("active", null));

        Assert.False(row.CanCopy);
        Assert.True(row.CanRevoke);
    }

    [Theory]
    [InlineData("revoked")]
    [InlineData("expired")]
    public void InactiveRows_OfferNeitherCopyNorRevoke(string state)
    {
        var row = ShareLinkFormat.MapRow(Summary(state, null));

        Assert.False(row.CanCopy);
        Assert.False(row.CanRevoke);
    }

    [Fact]
    public void UnknownValues_PaintRaw()
    {
        Assert.Equal("2w", ShareLinkFormat.ExpiryLabel("2w"));
        Assert.Equal("paused", ShareLinkFormat.StateLabel("paused"));
    }

    [Fact]
    public void KnownValues_ResolveThroughTheSharedMaps()
    {
        Assert.NotEqual("7d", ShareLinkFormat.ExpiryLabel("7d"));
        Assert.NotEqual("revoked", ShareLinkFormat.StateLabel("revoked"));
    }

    [Fact]
    public void OwnError_RepeatsOnlyTheSurfacesOwnKeys()
    {
        var create = new LocalizedText("share_link.error_create", new());
        var unrelated = new LocalizedText("media.error_upload", new());

        Assert.NotNull(ShareLinkFormat.OwnError(create, ShareLinkFormat.CreateErrorKeys));
        Assert.Null(ShareLinkFormat.OwnError(create, ShareLinkFormat.ListErrorKeys));
        Assert.Null(ShareLinkFormat.OwnError(unrelated, ShareLinkFormat.CreateErrorKeys));
        Assert.Null(ShareLinkFormat.OwnError(null, ShareLinkFormat.CreateErrorKeys));
    }
}
