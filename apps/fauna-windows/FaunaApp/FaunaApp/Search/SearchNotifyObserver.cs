using System.ComponentModel;
using Microsoft.UI.Dispatching;
using uniffi.fauna_client_search;

namespace FaunaApp.Search;

/// <summary>
/// Bridge between the UniFFI <see cref="SearchSnapshotObserver"/> trait (called
/// from arbitrary threads on the Rust side — the manager notifies after every
/// state mutation, including async WS-RPC completions on a tokio worker) and
/// WinUI's <see cref="INotifyPropertyChanged"/> (which must fire on the UI
/// thread). Exact mirror of <see cref="FaunaApp.Feed.FeedNotifyObserver"/>: the
/// Search page registers this with the shared <c>FfiSearchManager</c> and
/// re-reads <c>Snapshot()</c> on each tick (search.md § State &amp; data shape —
/// observer-driven rendering, no client-side query/paging state).
/// </summary>
internal sealed class SearchNotifyObserver : SearchSnapshotObserver, INotifyPropertyChanged
{
    private readonly DispatcherQueue? _dispatcher;

    public event PropertyChangedEventHandler? PropertyChanged;

    public SearchNotifyObserver()
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
