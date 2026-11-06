using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// A disposed <see cref="NestRpcClient"/> is silent (<c>apps/windows.md</c> § Client
/// lifetime). Every actor change disposes the outgoing client, and its
/// <c>Disconnect()</c> makes the connection-state watch yield a last
/// <c>Disconnected</c>. Delivered, that event reaches the outgoing <c>MainViewModel</c>
/// and writes the PROCESS-WIDE <c>OfflineGate.Shared</c> — so a second login with no
/// reset could see the old client's late <c>Disconnected</c> grey every control after the
/// new client had already connected.
///
/// <para>Both halves of the race are pinned: a raise made after the dispose, and one
/// posted to the UI context BEFORE it and still queued when the dispose lands (the
/// connection-state pump posts from its own thread, so the UI turn that delivers the
/// post can come after <c>DisposeAsync</c> ran).</para>
/// </summary>
public class NestRpcClientDisposeTests
{
    /// <summary>A UI context that queues every post until the test drains it.</summary>
    private sealed class QueuedContext : SynchronizationContext
    {
        private readonly Queue<(SendOrPostCallback Callback, object? State)> _posted = new();

        public override void Post(SendOrPostCallback d, object? state) => _posted.Enqueue((d, state));

        public int Pending => _posted.Count;

        public void Drain()
        {
            while (_posted.TryDequeue(out var item)) item.Callback(item.State);
        }
    }

    private static NestRpcClient ClientOn(QueuedContext context)
    {
        var rpc = new NestRpcClient("https://nest.example", new MockCryptoService());
        var previous = SynchronizationContext.Current;
        SynchronizationContext.SetSynchronizationContext(context);
        try
        {
            rpc.CaptureUiContext();
        }
        finally
        {
            SynchronizationContext.SetSynchronizationContext(previous);
        }
        return rpc;
    }

    [Fact]
    public async Task ARaiseAfterTheDisposeReachesNoSubscriber()
    {
        var rpc = new NestRpcClient("https://nest.example", new MockCryptoService());
        var seen = new List<FfiConnectionState>();
        var notices = 0;
        rpc.ConnectionStateChanged += seen.Add;
        rpc.StoreChanged += () => notices++;

        await rpc.DisposeAsync();
        rpc.RaiseConnectionStateOnUi(FfiConnectionState.Disconnected);
        rpc.RaiseStoreChangedOnUi();

        Assert.Empty(seen);
        Assert.Equal(0, notices);
    }

    [Fact]
    public async Task ARaisePostedBeforeTheDisposeIsDroppedAtDelivery()
    {
        var ui = new QueuedContext();
        var rpc = ClientOn(ui);
        var seen = new List<FfiConnectionState>();
        rpc.ConnectionStateChanged += seen.Add;

        rpc.RaiseConnectionStateOnUi(FfiConnectionState.Disconnected);
        Assert.Equal(1, ui.Pending);
        await rpc.DisposeAsync();
        ui.Drain();

        Assert.Empty(seen);
    }

    [Fact]
    public void ALiveClientStillDeliversThroughItsUiContext()
    {
        var ui = new QueuedContext();
        var rpc = ClientOn(ui);
        var seen = new List<FfiConnectionState>();
        rpc.ConnectionStateChanged += seen.Add;

        rpc.RaiseConnectionStateOnUi(FfiConnectionState.Connected);
        Assert.Empty(seen); // posted, not inline
        ui.Drain();

        Assert.Equal(new[] { FfiConnectionState.Connected }, seen);
    }
}
