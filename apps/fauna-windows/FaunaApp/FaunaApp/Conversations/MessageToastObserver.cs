using System.Linq;
using Microsoft.UI.Dispatching;
using FaunaApp.Core.Logs;
using FaunaApp.Services;
using uniffi.fauna_conversations;

namespace FaunaApp.Conversations;

/// <summary>
/// App-lifetime <see cref="SnapshotObserver"/> that fires an OS toast for each new
/// inbound direct message. Attached to the login-built <see cref="ConversationsManager"/>
/// in <c>App.xaml.cs</c> — deliberately NOT page-scoped (unlike
/// <see cref="ConversationsNotifyObserver"/>), because toasts must fire even when the
/// conversations page isn't open.
///
/// OS notifications split into a *when/for-whom* decision and a *how* (fire). Per
/// <c>conversations.md</c> § Where logic lives the **decision** lives once in shared
/// Rust — the <see cref="MessageNotificationTracker"/> UniFFI object
/// (<c>fauna_conversations</c>), unit-tested there and identical on every app
/// (priority #2/#4); this glue only projects the snapshot and calls the WinUI
/// <see cref="NotificationService"/> (the native toast). The tracker is internally
/// synchronized, but we still marshal the snapshot read + diff through the captured
/// <see cref="DispatcherQueue"/>: the manager fires <c>OnChanged</c> synchronously on
/// the mutator's thread mid-mutation, so reading <c>Snapshot()</c> re-entrantly there
/// could deadlock — enqueuing runs it after the mutation unwinds, on one thread (which
/// also keeps the stateful diff's tick order deterministic).
///
/// The e2e witness for <c>conversations</c> outcome 11 reads the shared fired-banner
/// log this observer feeds (<c>fauna_e2e_agent::MESSAGE_BANNERS_KEY</c>; the
/// recorders are <c>test-helpers</c> UniFFI seams, so every call sits behind
/// <c>#if DEBUG || FAUNA_E2E_AGENT</c> — the production FFI flavor has no such
/// symbol). <c>BannerPassStarted</c> precedes the snapshot read this tick will diff
/// and <c>BannerPassCompleted</c> follows its last fire, so a negative read can wait
/// for a tick that began after the message was planted.
/// </summary>
internal sealed class MessageToastObserver : SnapshotObserver
{
    private readonly ConversationsManager _manager;
    private readonly DispatcherQueue? _dispatcher;
    private readonly MessageNotificationTracker _tracker = new();
    private readonly object _lock = new();

    /// <param name="dispatcher">The UI-thread queue the diff is marshalled through.
    /// Defaults to the constructing thread's — right for the production login block,
    /// which runs on the UI thread. A caller that constructs off it (the e2e session
    /// builder, kicked off from the <c>set_state</c> command handler) MUST pass the
    /// window's queue: a null queue makes <see cref="OnChanged"/> diff inline on the
    /// mutator's thread, the re-entrant <c>Snapshot()</c> the class doc warns about.</param>
    public MessageToastObserver(ConversationsManager manager, DispatcherQueue? dispatcher = null)
    {
        _manager = manager;
        _dispatcher = dispatcher ?? DispatcherQueue.GetForCurrentThread();
    }

    public void OnChanged()
    {
        if (_dispatcher is null)
        {
            DiffAndNotify();
            return;
        }
        _dispatcher.TryEnqueue(DiffAndNotify);
    }

    private void DiffAndNotify()
    {
        ThreadActivity[] toNotify;
        lock (_lock)
        {
#if DEBUG || FAUNA_E2E_AGENT
            FaunaConversationsMethods.BannerPassStarted();
#endif
            var snap = _manager.Snapshot();
            // The shared projection, never a field-by-field constructor: a field
            // the decision grows reaches every app with no per-app edit.
            var threads = snap.threads
                .Select(FaunaConversationsMethods.ThreadActivityFromSummary)
                .ToArray();
            toNotify = _tracker.Diff(threads, snap.selectedThreadId, snap.launchFloorMs);
        }
        foreach (var activity in toNotify)
        {
            if (!NotificationService.ShowMessageNotification(activity.label))
            {
                // Loud rather than silent, and NOT recorded below: the process has no
                // notification host (or the toast call threw), so no banner reached
                // the user. Recording here would let the e2e log claim a fire the app
                // never made — the failure the witness exists to catch.
                ShellLog.Warn("MessageToastObserver",
                    $"[banner] not raised for {activity.threadId}: the toast was not shown " +
                    "(NotificationService is not registered in this launch, or the call threw)");
                continue;
            }
#if DEBUG || FAUNA_E2E_AGENT
            // Recorded at the FIRING site, after every suppression above it, so the
            // log means "a toast was raised for this thread" and never "the tracker
            // returned this" (`fauna_e2e_agent::MESSAGE_BANNERS_KEY`).
            FaunaConversationsMethods.RecordFiredBanner(activity.threadId, activity.label);
#endif
        }
#if DEBUG || FAUNA_E2E_AGENT
        FaunaConversationsMethods.BannerPassCompleted();
#endif
    }
}
