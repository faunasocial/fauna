using System.ComponentModel;
using Microsoft.UI.Dispatching;
using uniffi.fauna_conversations;

namespace FaunaApp.Conversations;

/// <summary>
/// Bridge between the UniFFI <see cref="SnapshotObserver"/> trait (called
/// from arbitrary threads on the Rust side — backend pollers, the test
/// bridge's <c>inject_inbound_for_test</c> path, or the UI thread itself)
/// and WinUI's <see cref="INotifyPropertyChanged"/> (which must fire on
/// the UI thread). Mirror of <see cref="FaunaApp.Onboarding.NotifyObserver"/>.
/// </summary>
internal sealed class ConversationsNotifyObserver : SnapshotObserver, INotifyPropertyChanged
{
    private readonly DispatcherQueue? _dispatcher;

    public event PropertyChangedEventHandler? PropertyChanged;

    public ConversationsNotifyObserver()
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
