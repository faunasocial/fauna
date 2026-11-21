using FaunaApp.Core.Services;
using Microsoft.Windows.AppNotifications;
using Microsoft.Windows.AppNotifications.Builder;

namespace FaunaApp.Services;

/// <summary>
/// The sync-complete toast's payload (`file-sync.md` § Files Appear
/// Automatically, the desktop upload notification): a quiet notice that a
/// burst of uploads collapses into, as linux's (one fixed replace id, low
/// urgency, no sound hint) and macOS's (one fixed identifier, no sound) do.
/// Pulled out of <see cref="NotificationService.ShowSyncCompleteNotification"/>
/// so the built payload is testable without a notification host, the way
/// <see cref="KnockToastBuilder"/> is (<c>SyncCompleteToastBuilderTests</c>
/// links this file).
///
/// <para>Windows has no priority below <see cref="AppNotificationPriority.Default"/>
/// (the other value is <c>High</c>), so the notice keeps the default, and it is
/// silent. It keeps the platform's short banner and stays in the notification
/// centre like macOS's: an <c>Expiration</c> there would remove it, and the one
/// tag already bounds the centre to a single notice. It does not outlive a
/// reboot — a finished upload is no news by then.</para>
/// </summary>
internal static class SyncCompleteToastBuilder
{
    /// <summary>The one tag — and group — every sync-complete toast carries, so
    /// each new one replaces the last instead of stacking one per file: macOS's
    /// request identifier, spelled the same.</summary>
    internal const string Tag = "sync-complete";

    /// <summary>The toast for one finished upload of <paramref name="fileName"/>,
    /// its text from the shared strings (<c>notifications/sync_complete_title</c>,
    /// <c>notifications/sync_complete_body</c> — linux's and macOS's). <c>AddText</c>
    /// escapes toast XML itself, so the file name is passed through as-is.</summary>
    public static AppNotification Build(string fileName)
    {
        var toast = new AppNotificationBuilder()
            .AddText(Strings.Get("notifications/sync_complete_title"))
            .AddText(Strings.Format("notifications/sync_complete_body", fileName))
            .MuteAudio()
            .SetTag(Tag)
            .SetGroup(Tag)
            .BuildNotification();
        toast.ExpiresOnReboot = true;
        return toast;
    }
}
