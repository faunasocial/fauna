using System;
using System.IO;
using System.Threading;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Explorer Share hand-off's app leg (windows.md § Shell Extension → <i>The Share
/// hand-off</i>, step 3): a second launch carrying a <c>fauna://</c> route forwards it to
/// the running instance, which receives exactly that route. Driven headlessly on real
/// named events and a real file, each test on its own unique names.
/// </summary>
public class RouteHandoffEndpointTests : IDisposable
{
    private readonly string _eventName = @"Local\FaunaApp-OpenRoute-test-" + Guid.NewGuid().ToString("N");
    private readonly string _dir = Path.Combine(Path.GetTempPath(), "fauna-route-" + Guid.NewGuid().ToString("N"));
    private string Pending => Path.Combine(_dir, RouteHandoffEndpoint.PendingFileName);

    public void Dispose()
    {
        try { Directory.Delete(_dir, recursive: true); } catch { /* best effort */ }
    }

    [Fact]
    public void AForwardedRoute_ReachesTheRunningInstance_AndIsConsumed()
    {
        const string uri = "fauna://share-link?folder=7&path=photos%2Fholiday.txt";
        string? received = null;
        using var got = new ManualResetEventSlim();
        using var endpoint = new RouteHandoffEndpoint(_eventName, Pending, r => { received = r; got.Set(); });
        Assert.True(endpoint.Start());

        Assert.True(RouteHandoffEndpoint.TryForward(_eventName, Pending, uri));

        Assert.True(got.Wait(TimeSpan.FromSeconds(10)), "the listener never received the route");
        Assert.Equal(uri, received);
        Assert.False(File.Exists(Pending), "a taken route must not linger for a later launch");
    }

    /// The whole app leg in one: the URI the shell mints (the shared builder)
    /// crosses the forward and parses back — through the same shared grammar the
    /// app applies — to exactly the target the agent named.
    [Fact]
    public void AForwardedShellRoute_ParsesToTheTargetTheAgentNamed()
    {
        var uri = uniffi.fauna_ffi.FaunaFfiMethods.AppRouteUri(
            new uniffi.fauna_core.AppRoute.ShareLink(42, "photos/Sommer & Sol.jpg"));
        string? received = null;
        using var got = new ManualResetEventSlim();
        using var endpoint = new RouteHandoffEndpoint(_eventName, Pending, r => { received = r; got.Set(); });
        Assert.True(endpoint.Start());
        Assert.True(RouteHandoffEndpoint.TryForward(_eventName, Pending, uri));
        Assert.True(got.Wait(TimeSpan.FromSeconds(10)));

        var route = uniffi.fauna_ffi.FaunaFfiMethods.ParseAppRoute(received!);
        var link = Assert.IsType<uniffi.fauna_core.AppRoute.ShareLink>(route);
        Assert.Equal(42, link.@folderId);
        Assert.Equal("photos/Sommer & Sol.jpg", link.@path);
    }

    [Fact]
    public void WithNobodyListening_TheForwardFails_AndWritesNothing()
    {
        Assert.False(RouteHandoffEndpoint.TryForward(_eventName, Pending, "fauna://folder-share?folder=3"));
        Assert.False(File.Exists(Pending));
    }

    [Fact]
    public void Take_ReadsOnce()
    {
        Directory.CreateDirectory(_dir);
        File.WriteAllText(Pending, "fauna://folder-share?folder=3\n");

        Assert.Equal("fauna://folder-share?folder=3", RouteHandoffEndpoint.Take(Pending));
        Assert.Null(RouteHandoffEndpoint.Take(Pending));
    }
}
