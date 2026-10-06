using System;
using Microsoft.UI.Dispatching;
using uniffi.fauna_media_machine;

namespace FaunaApp.Sync;

/// <summary>
/// Bridge between the UniFFI <see cref="MediaObserver"/> callback (fired from
/// arbitrary Tokio threads — notably the async <c>Refresh()</c> worker driving
/// <c>fauna.media.list</c>) and the WinUI UI thread, which must own every render +
/// bound-state mutation (off-thread mutation throws a silent <c>COMException</c>).
/// Mirrors <see cref="DevicesNotifyObserver"/>: capture the dispatcher at
/// construction (the page builds the machine on the UI thread) and marshal each
/// <c>OnChanged()</c> tick back to it before invoking the page's re-render off the
/// fresh <c>MediaPageSnapshot</c> (media.md rule 2 — observer-driven rendering).
/// </summary>
internal sealed class MediaNotifyObserver : MediaObserver
{
    private readonly DispatcherQueue? _dispatcher;
    private readonly Action _onChanged;

    public MediaNotifyObserver(Action onChanged)
    {
        _onChanged = onChanged;
        _dispatcher = DispatcherQueue.GetForCurrentThread();
    }

    public void OnChanged()
    {
        if (_dispatcher is null)
        {
            // No dispatcher (test / shutdown) — caller owns thread affinity.
            _onChanged();
            return;
        }
        _dispatcher.TryEnqueue(() => _onChanged());
    }
}
