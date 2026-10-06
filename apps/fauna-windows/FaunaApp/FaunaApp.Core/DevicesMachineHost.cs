using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_conversations;
using uniffi.fauna_devices_machine;
using uniffi.fauna_ffi;

namespace FaunaApp.Core;

/// <summary>
/// The ONE <see cref="DevicesMachine"/> for the current session, shared by the
/// Settings → Devices and Settings → Folders pages — the shape every other app
/// has (linux one over both sub-pages, tui one per sign-in, web's
/// <c>devices-session.ts</c>, FaunaKit's app-scene <c>devicesVM</c>). The twin of
/// <see cref="FeedManagerHost"/>, and built the same way.
///
/// <para><b>Why not one per page visit any more.</b> Both pages used to build a
/// fresh machine in <c>Page_Loaded</c> (neither has a <c>NavigationCacheMode</c>,
/// so every navigation is a fresh page). That machine's memory is load-bearing:
/// the followed-folders source answers a read that cannot reach the nest with
/// the LAST rows it read (<c>ui/folders.md</c> § Following a public folder — a
/// network fault leaves every followed row, and its last verdict, standing), and
/// a machine born inside the gap has none, so a revisit during a dropped
/// connection emptied the followed list. It also reset the refresh barrier
/// (<c>fauna_e2e_agent::DEVICES_REFRESHES_KEY</c>) under any baseline taken on
/// the previous visit.</para>
///
/// <para><b>What stays per page.</b> Each page keeps its own visit-scoped work —
/// its first <c>Refresh()</c>, its push handlers, its capability reads — and
/// listens through <see cref="Listen"/> while it is showing. Only the machine,
/// its one-time seam wiring and the observer fan-out live here.</para>
///
/// <para><b>Rebuild is keyed on the transport</b>, as for the feed: a re-auth
/// installs a fresh <c>INestRpcClient</c>, so the next page load rebuilds;
/// <see cref="ResetForActorChange"/> is the explicit actor boundary
/// (<c>App.DropActorScopedState</c>).</para>
/// </summary>
internal static class DevicesMachineHost
{
    private static readonly SemaphoreSlim Gate = new(1, 1);

    private static DevicesMachine? _current;

    /// <summary>The transport the cached machine was built over (the rebuild
    /// key, compared by reference).</summary>
    private static object? _builtOver;

    private static readonly FanOut Observers = new();

    /// <summary>The session's live machine, or <c>null</c> before either page
    /// has built one and after an actor change. Readers only (the e2e state
    /// serializer's refresh barrier).</summary>
    public static DevicesMachine? Current => _current;

    /// <summary>
    /// Route the machine's change notifications to <paramref name="observer"/>
    /// until the returned handle is disposed. Call it BEFORE awaiting
    /// <see cref="GetOrBuildAsync"/> and dispose it when the page is navigated
    /// away from, so a page left mid-build never keeps a listener.
    /// </summary>
    public static IDisposable Listen(DevicesObserver observer) => Observers.Add(observer);

    /// <summary>
    /// The session's machine, built and wired on first use over
    /// <paramref name="rpc"/> and reused by every later page load. Every seam is
    /// wired here, once, before the machine is handed out — the MLS join-filter
    /// (best-effort on <paramref name="convSession"/>, as before), the
    /// foreign-set (cross-nest) list source, and the followed-public-folder
    /// source. A failed build is not cached: the next page load retries.
    /// </summary>
    public static async Task<DevicesMachine> GetOrBuildAsync(
        INestRpcClient rpc, ConversationsSession? convSession)
    {
        if (_current is { } live && ReferenceEquals(_builtOver, rpc)) return live;

        // Page_Loaded is `async void` on the UI thread and both pages build, so
        // two loads can be in flight at once; without the gate each would build
        // and wire its own machine — the split instance this host exists to end.
        await Gate.WaitAsync();
        try
        {
            if (_current is { } raced && ReferenceEquals(_builtOver, rpc)) return raced;
            var machine = await rpc.BuildDevicesMachineAsync(Observers);
            // Without it DevicesMachine's Rust-side fail-safe drops EVERY
            // role == "member" row (folders.md § Sharing — Member
            // list-visibility); a missing session just leaves shared-with-me
            // sets invisible, never a page error.
            if (convSession is not null)
                FaunaFfiMethods.WireDevicesMlsQuery(machine, convSession);
            // Unwired, a set shared from ANOTHER nest has no row and renders as
            // absent, not stale.
            await rpc.WireDevicesForeignSetsAsync(machine);
            // Unwired, the followed list is permanently empty.
            await rpc.WireDevicesFollowedFoldersAsync(machine);
            _current = machine;
            _builtOver = rpc;
            return machine;
        }
        finally
        {
            Gate.Release();
        }
    }

    /// <summary>
    /// End the outgoing identity's machine at an actor change — a switch, a
    /// sign-out or a factory-reset re-onboard — so nothing reads the outgoing
    /// actor's rows. Called only from <c>App.DropActorScopedState</c>.
    ///
    /// <para><b>Every page listener goes with it.</b> A page stops listening in its
    /// own <c>OnNavigatedFrom</c>, but a root-frame navigation (sign-out, the e2e
    /// <c>reset</c>) replaces <c>MainPage</c> without calling <c>OnNavigatedFrom</c>
    /// on the page inside its content frame — so, left to the pages, every ended
    /// session's Folders/Devices page stayed subscribed for the life of the
    /// process, and each devices tick re-rendered all of them on the UI thread.
    /// That cost grew with every reset until UIA could no longer resolve the
    /// app's window. No page of an ended
    /// session has anything left to show, so the boundary ends them all; a page
    /// of the next session listens afresh when it loads.</para>
    /// </summary>
    public static void ResetForActorChange()
    {
        _current = null;
        _builtOver = null;
        var ended = Observers.Clear();
        if (ended > 0)
            Logs.ShellLog.Info("Devices", $"actor change ended {ended} page listener(s)");
    }

    /// <summary>Tick the listeners exactly as a machine change does — the unit
    /// tests' stand-in for a live machine.</summary>
    internal static void NotifyListenersForTest() => Observers.OnChanged();

    /// <summary>The one observer the machine is built with: forwards each change
    /// to every page currently listening (each page's own observer marshals to
    /// its UI thread).</summary>
    private sealed class FanOut : DevicesObserver
    {
        private readonly object _lock = new();
        private readonly List<DevicesObserver> _listeners = new();

        public IDisposable Add(DevicesObserver observer)
        {
            lock (_lock) _listeners.Add(observer);
            return new Subscription(this, observer);
        }

        private void Remove(DevicesObserver observer)
        {
            lock (_lock) _listeners.Remove(observer);
        }

        /// <summary>Drop every listener; returns how many there were. A page's
        /// later <c>Dispose</c> of its subscription is then a harmless no-op.</summary>
        public int Clear()
        {
            lock (_lock)
            {
                var n = _listeners.Count;
                _listeners.Clear();
                return n;
            }
        }

        public void OnChanged()
        {
            DevicesObserver[] snapshot;
            lock (_lock) snapshot = _listeners.ToArray();
            foreach (var listener in snapshot) listener.OnChanged();
        }

        private sealed class Subscription(FanOut owner, DevicesObserver observer) : IDisposable
        {
            private int _disposed;

            public void Dispose()
            {
                if (Interlocked.Exchange(ref _disposed, 1) == 0) owner.Remove(observer);
            }
        }
    }
}
