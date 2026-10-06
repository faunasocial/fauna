using System.Threading;
using Xunit;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Guards the C# UniFFI callback-vtable registration for a cross-module callback
/// interface. <see cref="FfiFeedManager"/> lives in the <c>fauna_ffi</c> module
/// (the concrete façade over the generic <c>fauna_feed::FeedManager&lt;R&gt;</c>),
/// but its <see cref="FeedSnapshotObserver"/> callback interface lives in
/// <c>fauna_feed</c>. The callback vtable is registered with Rust only in
/// <c>fauna_feed._UniFFILib</c>'s static constructor, and obtaining/using the
/// manager touches <c>fauna_ffi._UniFFILib</c> — never <c>fauna_feed</c>'s. Before
/// the <c>CallbackInterfaceTemplate.cs</c> bindgen fix (the converter forces its
/// module's init when first lowered), the manager's first <c>notify()</c> panicked
/// <c>"Foreign pointer not set"</c> — which the windows app previously dodged with a
/// per-VM static-init workaround in <see cref="FaunaApp.Core.ViewModels.FeedViewModel"/>.
///
/// <para>The native <c>fauna_ffi</c> dll loads in the test host (memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>). <c>NestClient::new</c>
/// does not open a socket, so this builds fully offline.</para>
///
/// <para><b>Order note:</b> the red reproduces only when <c>fauna_feed</c> has not
/// already been initialized by another test (e.g. <c>FeedPostItemTests</c> calls
/// <c>ClassifySources</c>, a <c>fauna_feed</c> free fn, which inits the module). To
/// observe the pre-fix red, run this file alone:
/// <c>dotnet test --filter FullyQualifiedName~FeedObserverVtableTests</c>.</para>
/// </summary>
public class FeedObserverVtableTests
{
    private sealed class SpyObserver : FeedSnapshotObserver
    {
        public int Count;
        public void OnChanged() => Interlocked.Increment(ref Count);
    }

    [Fact]
    public void SyncMutation_FiresObserver_AcrossTheFfiBoundary()
    {
        // Offline build — NestClient::new opens no socket (mirrors the Rust
        // feed_manager unit test). A valid 32-byte secret builds the manager.
        using var nest = new FfiNestClient("wss://127.0.0.1:0/ws", new byte[32]);
        using var mgr = nest.FeedManager(new byte[32]);

        var spy = new SpyObserver();
        mgr.AddObserver(spy);

        // update_compose() is synchronous and calls notify() → on_changed()
        // straight back across the FFI into the foreign observer. Without the
        // fauna_feed vtable registered this panics "Foreign pointer not set".
        mgr.UpdateCompose("hello", "", null);

        Assert.Equal(1, spy.Count);
    }
}
