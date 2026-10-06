using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Services;
using uniffi.fauna_conversations;

namespace FaunaApp.Conversations;

/// <summary>
/// Feeds windows' home-screen widget — the taskbar badge (<see cref="TaskbarBadgeService"/>,
/// apps/windows.md § Home-screen widget) — from the shared conversations fold on every
/// conversations-plane change: the windows twin of the linux tray-toast loop's
/// <c>manager().unread_total()</c> read (<c>apps/fauna-linux/src/main.rs</c>). Registered
/// once per manager inside <see cref="ConversationsManagerHost"/>'s own factory, like
/// <see cref="RoomsRefreshObserver"/>, so it lives for the manager's whole lifetime with
/// no <c>ConversationsPage</c> open — the badge must move with the window hidden to the
/// tray, which is exactly when no page-scoped observer exists — and an actor change's
/// fresh manager gets a fresh observer whose first read reconciles the badge to the
/// incoming identity's count.
///
/// <para>The manager fires <c>OnChanged</c> synchronously on the mutator's thread, so
/// reading the thread store inline could re-enter a lock the mutation still holds (the
/// re-entrancy <see cref="MessageToastObserver"/>'s class doc warns about). This observer
/// therefore reads on the thread pool, after the notify returns, and coalesces: a burst
/// of changes schedules one read, and a change landing during the read schedules the
/// next. The number is never computed here — <c>UnreadTotal()</c> is the shared
/// <c>fauna_conversations::sum_unread</c> over every thread of the account (priority #2),
/// so this glue holds no tally of its own.</para>
/// </summary>
internal sealed class UnreadBadgeObserver : SnapshotObserver
{
    private readonly ConversationsManager _manager;
    private int _pending;

    public UnreadBadgeObserver(ConversationsManager manager)
    {
        _manager = manager;
    }

    public void OnChanged()
    {
        if (Interlocked.Exchange(ref _pending, 1) == 1) return;
        _ = Task.Run(ReadAndPublish);
    }

    private void ReadAndPublish()
    {
        Interlocked.Exchange(ref _pending, 0);
        uint total;
        try { total = _manager.UnreadTotal(); }
        catch { return; /* a manager mid-teardown at an actor change — the successor's observer reconciles */ }
        TaskbarBadgeService.Publish(total);
    }
}
