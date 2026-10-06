using System;
using Microsoft.UI.Dispatching;
using uniffi.fauna_devices_machine;

namespace FaunaApp.Sync;

/// <summary>
/// Bridge between the UniFFI <see cref="DevicesObserver"/> callback (fired from
/// arbitrary Tokio threads — notably the async <c>Refresh()</c> / gesture
/// workers) and the WinUI UI thread, which must own every render + bound-state
/// mutation (off-thread mutation throws a silent <c>COMException</c>). Mirrors
/// <see cref="FolderWizardNotifyObserver"/>: capture the dispatcher at
/// construction (the page builds the machine on the UI thread) and marshal each
/// <c>OnChanged()</c> tick back to it before invoking the page's re-render.
/// </summary>
internal sealed class DevicesNotifyObserver : DevicesObserver
{
    private readonly DispatcherQueue? _dispatcher;
    private readonly Action _onChanged;

    public DevicesNotifyObserver(Action onChanged)
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
