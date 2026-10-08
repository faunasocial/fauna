using System;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The by-path content GET fetches a bridged post's picture from the user's OWN nest
/// with the session bearer (<c>docs/goal/architecture/render-model.md</c> § D6c). The
/// path comes out of a post's document, so the client that attaches the bearer must
/// refuse anything that would resolve to another origin — the same rule the shared
/// fold applies before it emits a <c>ProxiedImage</c> (<c>fauna-feed</c>'s
/// <c>proxied_media_path</c>: rooted at one <c>/</c>, never <c>//</c>), held again here
/// because this is where a mistake would send the bearer elsewhere.
/// </summary>
public class NestContentPathTests
{
    [Theory]
    [InlineData("/api/v1/bluesky/media?url=https%3A%2F%2Fcdn.bsky.app%2Fimg%2Fa%40jpeg")]
    [InlineData("/api/v1/media/proxy?url=https%3A%2F%2Fr.example%2Fa.png")]
    public void A_nest_relative_path_is_accepted(string path)
        => Assert.True(NestContentPath.IsNestRelative(path));

    [Theory]
    [InlineData("https://elsewhere.example/a.png")]  // absolute: another origin
    [InlineData("//elsewhere.example/a.png")]        // scheme-relative: another origin
    [InlineData("/\\elsewhere.example/a.png")]       // a backslash a URL parser may read as '/'
    [InlineData("api/v1/media/proxy?url=x")]         // not rooted
    [InlineData(" /api/v1/media/proxy?url=x")]
    [InlineData("")]
    [InlineData(null)]
    public void Anything_else_is_refused(string? path)
        => Assert.False(NestContentPath.IsNestRelative(path));

    [Fact]
    public async Task The_bearer_carrying_GET_refuses_a_path_that_is_not_nest_relative()
    {
        // DirectNestClient's own refusal, raised before it authenticates or dials.
        var client = new DirectNestClient("https://127.0.0.1:443", new MockCryptoService());

        await Assert.ThrowsAsync<ArgumentException>(
            () => client.GetContentAsync("https://elsewhere.example/a.png"));
    }
}
