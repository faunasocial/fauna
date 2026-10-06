using System.Runtime.CompilerServices;
using Xunit;

namespace FaunaApp.Tests;

// `ConnectedAsync` caches its minted `FfiNestClient` only after `await
// nest.Connect()` returns; `DisposeAsync` disconnects `_nest` before tearing
// down, but a `DisposeAsync` racing an in-flight `ConnectedAsync` sees `_nest`
// still null (it is only written after `Connect()` returns) and skips the
// disconnect — then `ConnectedAsync` stores the now-connected client into an
// already torn-down wrapper, orphaning its reconnect supervisor, and
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
// `FfiNestClient.Connect()` dials a real WS-RPC socket with no bare
// constructor and no injectable seam (`NestRpcPushDispatchTests.Subject()`'s
// own comment: "No connection is made ... DispatchPush is pure routing"), so
// no test in this project can actually interleave a `Connect()` in flight
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

        var connect = RequireIndex(body, "await nest.Connect()");
        var disposedGuard = RequireIndex(body, "if (_disposed)", connect,
            "ConnectedAsync no longer re-checks _disposed after Connect() returns — a DisposeAsync racing an in-flight connect would cache the client onto an already torn-down wrapper");
        var disconnect = RequireIndex(body, "await nest.Disconnect()", disposedGuard,
            "the disposed branch must disconnect the client it minted, not just throw — dropping it orphans its reconnect supervisor");
        var dispose = RequireIndex(body, "nest.Dispose()", disconnect);
        var throwStmt = RequireIndex(body, "throw new ObjectDisposedException", dispose);
        var write = RequireIndex(body, "_nest = nest;");

        Assert.True(throwStmt < write,
            "the disposed branch must run (and throw) before _nest is written, or a disposed wrapper ends up caching a live client");
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
