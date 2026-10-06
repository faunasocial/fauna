using System;
using Microsoft.UI.Dispatching;
using uniffi.fauna_backups_machine;

namespace FaunaApp.Sync;

/// <summary>
/// Marshals <c>BackupsMachine</c> observer ticks onto the UI thread — the
/// <see cref="DevicesNotifyObserver"/> twin for the Backups page's snapshot half
/// (<c>docs/goal/ui/backups.md</c> § Snapshot-list shape). The machine ticks from
/// whatever thread its async gesture completed on; every consumer here mutates
/// bound XAML state, which WinUI requires on the dispatcher thread.
/// </summary>
internal sealed class BackupsNotifyObserver : BackupsObserver
{
    private readonly DispatcherQueue? _dispatcher;
    private readonly Action _onChanged;

    public BackupsNotifyObserver(Action onChanged)
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
