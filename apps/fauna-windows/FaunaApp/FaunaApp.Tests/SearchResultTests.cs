using System.Collections.Generic;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_search;
using uniffi.fauna_core;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <see cref="SearchResult.FromRow"/> plumbing. Before the manager adoption
/// (search.md § Implementation status today, 2026-08-05) this class locked
/// windows to the shared-Rust badge map / snippet cleanup by calling the REAL
/// <c>FaunaFfiMethods.SearchContentTypeBadge</c>/<c>SearchCleanSnippet</c>
/// exports per row; that whole cross-language-conformance concern is now
/// structurally moot — <c>FfiSearchManager</c> computes both fields
/// server-side inside <c>SearchResultRow</c> before the row ever reaches this
/// client, so windows cannot diverge from the shared map because it no longer
/// does the transform at all. What's left to lock down is only the display
/// projection: the badge resolves through the SAME <see cref="Strings"/>
/// pipeline every other <c>LocalizedText</c> on this page uses, the snippet
/// passes through verbatim, and the type icon (windows-only chrome) still
/// keys off the raw content type.
/// </summary>
[Collection("StringsGlobal")]
public class SearchResultTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["search_page/badge_post"] = "Post",
            ["search_page/badge_email"] = "Email",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public SearchResultTests() => Strings.Initialize(new FakeLocalizer());

    private static SearchResultRow Row(string contentType, string badgeKey, string snippet, long timestamp) =>
        new(
            contentId: "doc-key",
            contentType: contentType,
            badge: new LocalizedText(badgeKey, new Dictionary<string, string>()),
            snippet: snippet,
            timestamp: timestamp,
            source: SearchSource.Nest,
            navigation: null);

    [Fact]
    public void FromRow_ResolvesTheBadgeThroughTheSharedStringsPipeline()
    {
        var result = SearchResult.FromRow(Row("post", "search_page/badge_post", "", 0));
        Assert.Equal("Post", result.Title);
    }

    [Fact]
    public void FromRow_ResolvesAnUnknownKeyByPassingItThrough()
    {
        // Mirrors Strings.Resolve's fallback for a key the fake localizer never
        // heard of — matches how a real .resw lookup miss behaves too.
        var result = SearchResult.FromRow(Row("widget", "search_page/badge_widget", "", 0));
        Assert.Equal("search_page/badge_widget", result.Title);
    }

    [Fact]
    public void FromRow_PassesTheManagerCleanedSnippetThroughVerbatim()
    {
        // The manager already ran clean_snippet — no FTS markers reach this
        // client for FromRow to strip.
        var result = SearchResult.FromRow(Row("post", "search_page/badge_post", "a hit & more", 0));
        Assert.Equal("a hit & more", result.Snippet);
    }

    [Fact]
    public void FromRow_KeepsTheContentTypeForTheWindowsOnlyTypeIcon()
    {
        Assert.Equal("post", SearchResult.FromRow(Row("post", "search_page/badge_post", "", 0)).Type);
    }
}
