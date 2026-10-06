using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using Microsoft.Windows.AppNotifications;
using Microsoft.Windows.AppNotifications.Builder;
using uniffi.fauna_ffi;

namespace FaunaApp.Services;

public static class NotificationService
{
    private static bool _initialized;

    public static void Initialize()
    {
        if (_initialized) return;
        try
        {
            AppNotificationManager.Default.Register();
            _initialized = true;
        }
        catch (Exception ex)
        {
            // Notifications may not be available in this launch shape; the app runs on
            // without toasts. Said in the log rather than swallowed: a DM banner the
            // user was never shown is otherwise undiagnosable, and the e2e witness's
            // failure message folds every "banner" line of the app log in (`_why`).
            ShellLog.Warn("NotificationService",
                $"[banner] toasts unavailable: AppNotificationManager.Register() threw {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>
    /// Raises the "New Message" toast and returns whether
    /// <c>AppNotificationManager.Show</c> actually ran — <c>false</c> when
    /// <see cref="Initialize"/> never registered a notification host (notifications
    /// unavailable in this launch shape) or the call itself threw. The DM-toast
    /// observer records a fired banner on the e2e log only on <c>true</c>: an entry
    /// there means the user was handed a toast, so a recorder that ran ahead of
    /// this guard would log banners the process never raised
    /// (<c>conversations.md</c> § Where logic lives → <em>New-message OS-toast
    /// decision</em>; apple's <c>postMessageNotification</c> returns the same
    /// bool for the same reason).
    /// </summary>
    public static bool ShowMessageNotification(string senderName)
    {
        if (!_initialized) return false;
        try
        {
            var builder = new AppNotificationBuilder()
                .AddText("New Message")
                .AddText($"From {senderName}");
            AppNotificationManager.Default.Show(builder.BuildNotification());
            return true;
        }
        catch { return false; }
    }

    /// <summary>
    /// The title is always <c>notifications/knock_title</c>; the body is the
    /// shared <c>knock_text_for</c> decision, resolved and sanitized by
    /// <see cref="KnockToastText.For"/> (`notifications.md` § Localized body
    /// → *The knock toast*) — never the raw sender id alone. Tagged with the
    /// sender's 8-hex prefix (the same prefix the fallback sentence names) so
    /// a repeat knock from the same stranger replaces the earlier toast
    /// instead of stacking one per fresh key.
    /// </summary>
    internal static void ShowKnockNotification(FfiKnock knock)
    {
        if (!_initialized) return;
        try
        {
            var (title, body) = KnockToastText.For(knock);
            var tag = knock.senderId.Length > 8 ? knock.senderId[..8] : knock.senderId;
            var builder = new AppNotificationBuilder()
                .AddText(title)
                .AddText(body)
                .SetTag(tag);
            AppNotificationManager.Default.Show(builder.BuildNotification());
        }
        catch { }
    }

    /// <summary>The local sync-agent's <c>FileStatusChanged{Synced}</c> push
    /// (`sync-agent.md` § Local agent health — the sync-complete toast consumer),
    /// surfaced via the shared listener thread <c>SyncAgentSession</c> holds
    /// (<c>fauna_ipc::events</c>, the same loop the linux GTK client runs).</summary>
    public static void ShowSyncCompleteNotification(string fileName)
    {
        if (!_initialized) return;
        try
        {
            var builder = new AppNotificationBuilder()
                .AddText("Sync complete")
                .AddText(fileName);
            AppNotificationManager.Default.Show(builder.BuildNotification());
        }
        catch { }
    }
}
