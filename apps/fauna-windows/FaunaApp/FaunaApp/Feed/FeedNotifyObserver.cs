using System.ComponentModel;
using Microsoft.UI.Dispatching;
using uniffi.fauna_feed;

namespace FaunaApp.Feed;

/// <summary>
/// Bridge between the UniFFI <see cref="FeedSnapshotObserver"/> trait (called
/// from arbitrary threads on the Rust side — the manager notifies after every
/// state mutation, including async WS-RPC completions on a tokio worker) and
/// WinUI's <see cref="INotifyPropertyChanged"/> (which must fire on the UI
/// thread). Exact mirror of <see cref="FaunaApp.Conversations.ConversationsNotifyObserver"/>:
/// the Feed page registers this with the shared <c>FfiFeedManager</c> and
/// re-reads <c>Snapshot()</c> on each tick (feed.md § Architectural rules —
/// observer-driven rendering, no client-side post-list state).
/// </summary>
internal sealed class FeedNotifyObserver : FeedSnapshotObserver, INotifyPropertyChanged
{
    private readonly DispatcherQueue? _dispatcher;

    public event PropertyChangedEventHandler? PropertyChanged;

    public FeedNotifyObserver()
    {
        _dispatcher = DispatcherQueue.GetForCurrentThread();
    }

    public void OnChanged()
    {
        if (_dispatcher is null)
        {
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(string.Empty));
            return;
        }

        _dispatcher.TryEnqueue(() =>
        {
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(string.Empty));
        });
    }
}
