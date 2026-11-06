using System.Runtime.CompilerServices;
using Xunit;

namespace FaunaApp.Tests;

// `ConnectedAsync` caches its minted `FfiNestClient` only after `await
// nest.EnsureConnected()` returns; `DisposeAsync` disconnects `_nest` before tearing
// down, but a `DisposeAsync` racing an in-flight `ConnectedAsync` sees `_nest`
// still null (it is only written after `EnsureConnected()` returns) and skips the
// disconnect — then `ConnectedAsync` stores the now-supervised client into an
// already torn-down wrapper, orphaning its reconnect supervisor (closed by
// publishing first and re-checking `_disposed` after, each side fenced), and
// `DisposeAsync`'s `_connectGate.Dispose()` (which does not wait for a
// holder to release) can make the in-flight call's `_connectGate.Release()`
// throw `ObjectDisposedException` on its way out.
// account-scoping.md's in-memory corollary ("background loops … retired by
// the same drop") and transport-connection.md ("no dialer outlives its
// owner") both name this class of hazard; the apple twin is
// `APIClient.ensureNestConnected`'s post-`connect()` `stillThisActor()`
// guard, fixed for the identical race
// .
//
// `FfiNestClient.EnsureConnected()` dials a real WS-RPC socket with no bare
// constructor and no injectable seam (`NestRpcPushDispatchTests.Subject()`'s
// own comment: "No connection is made ... DispatchPush is pure routing"), so
// no test in this project can actually interleave one in flight
// with a `DisposeAsync` call. The guard's presence and ordering are pinned
// STRUCTURALLY instead — this file is windows' twin of apple's
// `APIClientActorAdoptionTests.ensureNestConnectedNeverStrandsAClient` /
// `adoptActorDisconnectsTheOutgoingNestClient`, which make the identical
// split for the identical hazard.
public class NestRpcClientConnectDisposeRaceTests
{
    private static readonly string SourcePath = ResolveSourcePath();

    private static string ResolveSourcePath([CallerFilePath] string testFilePath = "") =>
        Path.GetFullPath(Path.Combine(
            Path.GetDirectoryName(testFilePath)!, // FaunaApp.Tests
            "..", "FaunaApp.Core", "Services", "NestRpcClient.cs"));

    private static string Slice(string startAnchor, string endAnchor)
    {
        var source = File.ReadAllText(SourcePath);
        var start = source.IndexOf(startAnchor, StringComparison.Ordinal);
        Assert.True(start >= 0, $"`{startAnchor}` moved or was renamed — update this test's anchor");
        var end = source.IndexOf(endAnchor, start, StringComparison.Ordinal);
        Assert.True(end >= 0, $"`{endAnchor}` no longer follows `{startAnchor}` — update this test's anchor");
        return source[start..end];
    }

    private static int RequireIndex(string body, string anchor, int from = 0, string? message = null)
    {
        var idx = body.IndexOf(anchor, from, StringComparison.Ordinal);
        Assert.True(idx >= 0, message ?? $"`{anchor}` not found — update this test's anchor");
        return idx;
    }

    [Fact]
    public void DisposeAsyncSetsTheDisposedFlagBeforeAnythingElse()
    {
        var source = File.ReadAllText(SourcePath);
        Assert.Contains(
            "public async ValueTask DisposeAsync()\n    {\n        _disposed = true;",
            source);
    }

    [Fact]
    public void ConnectedAsyncDisconnectsNeverCachesAClientWhoseWrapperWasDisposedMidConnect()
    {
        var body = Slice(
            "private async Task<FfiNestClient> ConnectedAsync()",
            "public async Task<int> KeypackageCountAsync()");

        var connect = RequireIndex(body, "await nest.EnsureConnected()");
        // Publish, THEN check — both fenced, mirroring DisposeAsync's own order (set
        // _disposed, then unpublish). Checking first left a window between the check and
        // the write in which a dispose saw null and a live client was then cached on a
        // torn-down wrapper.
        var publish = RequireIndex(body, "Interlocked.Exchange(ref _nest, nest);", connect,
            "ConnectedAsync must publish the minted client with a full fence (Interlocked.Exchange) after EnsureConnected() returns");
        var disposedGuard = RequireIndex(body, "if (_disposed)", publish,
            "ConnectedAsync no longer re-checks _disposed after publishing the client — a DisposeAsync racing an in-flight connect would leave the client cached on an already torn-down wrapper");
        var takeBack = RequireIndex(body, "Interlocked.CompareExchange(ref _nest, null, nest)", disposedGuard,
            "the disposed branch must take the client back out of the field before tearing it down, or the wrapper keeps a destroyed client");
        var disconnect = RequireIndex(body, "await nest.Disconnect()", takeBack,
            "the disposed branch must disconnect the client it minted, not just throw — dropping it orphans its reconnect supervisor");
        var dispose = RequireIndex(body, "nest.Dispose()", disconnect);
        RequireIndex(body, "throw new ObjectDisposedException", dispose,
            "the disposed branch must throw, never hand the caller a client of a disposed wrapper");
    }

    [Fact]
    public void ConnectedAsyncHandsOutNothingOnceDisposed()
    {
        var body = Slice(
            "private async Task<FfiNestClient> ConnectedAsync()",
            "public async Task<int> KeypackageCountAsync()");

        var refuse = RequireIndex(body, "if (_disposed) throw new ObjectDisposedException",
            message: "ConnectedAsync's entry must refuse a disposed wrapper before its cached-client fast path");
        var fastPath = RequireIndex(body, "Volatile.Read(ref _nest) is { } live", refuse,
            "the cached-client fast path must come after the disposed refusal, or a disposed wrapper hands out its destroyed client");
        Assert.True(refuse < fastPath);
    }

    [Fact]
    public void DisposeAsyncUnpublishesTheClientBeforeDestroyingIt()
    {
        var body = Slice(
            "public async ValueTask DisposeAsync()",
            "_connectGate.Dispose();");

        var unpublish = RequireIndex(body, "Interlocked.Exchange(ref _nest, null)",
            message: "DisposeAsync must take the client out of _nest before disconnecting and disposing it, so nothing reading the field meanwhile gets a destroyed client");
        RequireIndex(body, "nest.Dispose()", unpublish,
            "DisposeAsync must dispose the client it unpublished, after unpublishing it");
    }

    [Fact]
    public void ConnectedAsyncsGateReleaseSurvivesDisposeAsyncDisposingTheGateMidFlight()
    {
        var body = Slice(
            "private async Task<FfiNestClient> ConnectedAsync()",
            "public async Task<int> KeypackageCountAsync()");

        var finallyIdx = RequireIndex(body, "finally");
        var tryIdx = RequireIndex(body, "try", finallyIdx);
        var release = RequireIndex(body, "_connectGate.Release();", tryIdx);
        RequireIndex(body, "catch (ObjectDisposedException", release,
            "ConnectedAsync's gate release no longer catches ObjectDisposedException — DisposeAsync disposes the gate without waiting for a holder to release it, so Release() can throw once a dispose races an in-flight connect");
    }
}
