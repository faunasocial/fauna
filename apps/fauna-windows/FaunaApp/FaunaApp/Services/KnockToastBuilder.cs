using Microsoft.Windows.AppNotifications;
using Microsoft.Windows.AppNotifications.Builder;

namespace FaunaApp.Services;

/// <summary>
/// The knock toast's payload (`notifications.md` § Localized body → *The knock
/// toast*): title, body, and the one tag every knock shares. Pulled out of
/// <see cref="NotificationService.ShowKnockNotification"/> so the built payload
/// is testable without a notification host: it needs only the Windows App SDK's
/// builder, so a test project that references that SDK can link this file
/// without the WinUI project (<c>KnockToastBuilderTests</c> does).
/// </summary>
internal static class KnockToastBuilder
{
    /// <summary>Every knock toast carries this one tag, so a flood of fresh-key
    /// knocks replaces a single toast instead of stacking one per sender (linux
    /// <c>NOTIF_ID_KNOCK</c>, android <c>KNOCK_NOTIFICATION_ID</c>).</summary>
    internal const string Tag = "knock";

    /// <summary>The toast for (title, body). <c>AddText</c> escapes toast XML
    /// itself (checked 2026-10-11 by building a payload from <c>&lt;b&gt; &amp;</c>), so the knocker's text —
    /// already plain single-line text from the shared decision — is passed
    /// through as-is; escaping it here too would show literal <c>&amp;amp;</c>.</summary>
    public static AppNotification Build(string title, string body) =>
        new AppNotificationBuilder()
            .AddText(title)
            .AddText(body)
            .SetTag(Tag)
            .BuildNotification();
}
