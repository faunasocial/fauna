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
/// </summary>
internal static class TaskbarBadgeService
{
    private static readonly TaskbarBadgeTracker Tracker = new();
    private static volatile bool _unavailable;

    /// <summary>
    /// Push <paramref name="unreadTotal"/> to the taskbar badge, or clear it at zero.
    /// Safe from any thread (the badge updater is agile); cheap on a repeat.
    /// </summary>
    internal static void Publish(uint unreadTotal)
    {
        if (_unavailable) return;
        if (!Tracker.ShouldPush(unreadTotal)) return;
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
            _unavailable = true;
            ShellLog.Warn("TaskbarBadge",
                $"[badge] taskbar badge unavailable (no package identity in this launch shape?): "
                + $"{ex.GetType().Name}: {ex.Message}");
        }
    }
}
