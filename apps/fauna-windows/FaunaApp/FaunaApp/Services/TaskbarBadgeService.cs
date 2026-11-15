using System;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using Windows.Data.Xml.Dom;
using Windows.UI.Notifications;

namespace FaunaApp.Services;

/// <summary>
/// Windows' home-screen widget — the numeric badge on Fauna's taskbar button
/// (apps/windows.md § Home-screen widget). The OS paints it on the app's taskbar
/// icon whether or not a window is open, and keeps it across process exits until
/// the next update, which is what lets a tray-resident Fauna keep the count
/// current with nothing on screen (common.md § Home-screen widget, the promise's
/// second half).
///
/// <para>The decision (what to hand the OS, whether anything changed) is the pure
/// <see cref="TaskbarBadgePolicy"/> / <see cref="TaskbarBadgeTracker"/> pair in
/// FaunaApp.Core; this class is only the WinRT call —
/// <c>BadgeUpdateManager.CreateBadgeUpdaterForApplication</c>, the one badge API
/// Windows has.</para>
///
/// <para><b>Needs package identity.</b> Badge notifications are keyed on the app's
/// AUMID, so the updater throws <c>0x80070490</c> (element not found) in a process
/// with no package identity. The Store package is packaged, so it always has one;
/// the MSI channel's FaunaApp.exe gets one from the sparse identity package the MSI
/// registers (installers/windows.md § Package identity for FaunaApp.exe), and has none
/// when that package is absent — Explorer Integration deselected, or the plain e2e
/// launch of a build output. Without identity the badge is simply not painted and
/// nothing else changes: the first failure is logged once as <c>[badge]</c> (the e2e
/// witness folds the app log into its failure message) and every later push is
/// skipped, so a headless launch never pays a throwing WinRT call per snapshot.</para>
///
/// <para><b>One account at a time.</b> An actor change — sign-out, switch, factory
/// reset — calls <see cref="EndAccount"/> before the outgoing account's conversations
/// manager is swapped out (<c>ConversationsManagerHost.ResetForActorChange</c>): the
/// badge is cleared at once, and a reading that manager's observer still has in flight
/// is dropped by its <see cref="TaskbarBadgeTracker.Generation"/>. The decide-and-push
/// is one step under <see cref="Gate"/>, so such a reading can never land between the
/// clear and the next account's first push.</para>
/// </summary>
internal static class TaskbarBadgeService
{
    private static readonly TaskbarBadgeTracker Tracker = new();
    private static readonly object Gate = new();
    private static volatile bool _unavailable;

    /// <summary>
    /// The account the badge belongs to now. A reader of the unread fold takes it once,
    /// when it is built (<c>UnreadBadgeObserver</c>, one per conversations manager), and
    /// hands it to every <see cref="Publish"/>.
    /// </summary>
    internal static long Generation => Tracker.Generation;

    /// <summary>
    /// Push <paramref name="unreadTotal"/>, read for the account of
    /// <paramref name="generation"/>, to the taskbar badge, or clear it at zero; a
    /// reading of an account that has since ended is dropped. Safe from any thread (the
    /// badge updater is agile); cheap on a repeat.
    /// </summary>
    internal static void Publish(uint unreadTotal, long generation)
    {
        if (_unavailable) return;
        lock (Gate)
        {
            if (_unavailable) return;
            if (!Tracker.ShouldPush(unreadTotal, generation)) return;
            try
            {
                var updater = BadgeUpdateManager.CreateBadgeUpdaterForApplication();
                var payload = TaskbarBadgePolicy.Payload(unreadTotal);
                if (payload is null)
                {
                    updater.Clear();
                    return;
                }
                var xml = new XmlDocument();
                xml.LoadXml(payload);
                updater.Update(new BadgeNotification(xml));
            }
            catch (Exception ex)
            {
                MarkUnavailable(ex);
            }
        }
    }

    /// <summary>
    /// The signed-in account ended (sign-out, switch, factory reset): take its count off
    /// the taskbar now, drop every reading still in flight for it, and let the next
    /// account's first reading push whatever it is. Without this the OS would keep the
    /// signed-out account's count on the button — it holds a badge across process exits
    /// — because nothing reads the fold again until the next account's manager is built,
    /// and after a sign-out none is.
    /// </summary>
    internal static void EndAccount()
    {
        lock (Gate)
        {
            Tracker.EndAccount();
            if (_unavailable) return;
            try
            {
                BadgeUpdateManager.CreateBadgeUpdaterForApplication().Clear();
                ShellLog.Info("TaskbarBadge", "[badge] cleared: the signed-in account ended");
            }
            catch (Exception ex)
            {
                MarkUnavailable(ex);
            }
        }
    }

    private static void MarkUnavailable(Exception ex)
    {
        _unavailable = true;
        ShellLog.Warn("TaskbarBadge",
            $"[badge] taskbar badge unavailable (no package identity in this launch shape?): "
            + $"{ex.GetType().Name}: {ex.Message}");
    }
}
