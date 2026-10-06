using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// What the windows knock toast says — the shared <c>knock_text_for</c>
/// decision (`notifications.md` § Localized body → *The knock toast*),
/// resolved through this app's own RESW pipeline exactly like
/// <see cref="Strings.Resolve"/> does for the notifications list.
/// Pulled out of <c>NotificationService.ShowKnockNotification</c> (the WinUI
/// project) so it is testable without a notification host
/// (<c>notificationsAvailable</c>-style guards are always false in a
/// unit-test binary) — mirrors apple's <c>knockToastBody(for:)</c>.
/// </summary>
internal static class KnockToastText
{
    /// <summary>
    /// The toast's (title, body). The title is always the RESW
    /// <c>notifications/knock_title</c> sentence; the body is
    /// <c>knock_text_for(knock)</c>'s answer — the row's own localized
    /// sentence for a known body key, else the toast's own sentence naming
    /// the sender. Never <c>knock.summary</c> painted alone: that is the
    /// knocker's raw message, not an English rendering of the row. The
    /// knocker's args arrive already reduced to plain, capped single-line text
    /// by the shared decision (<c>sanitize_plain_line</c>), so nothing here
    /// re-sanitizes.
    /// </summary>
    public static (string Title, string Body) For(FfiKnock knock)
    {
        var title = Strings.Get("notifications/knock_title");
        var body = FaunaFfiMethods.KnockTextFor(knock) switch
        {
            FfiNotificationText.Localized l => Strings.Resolve(l.text),
            FfiNotificationText.Verbatim v => v.text,
            _ => Strings.Get("notifications/knock_body"),
        };
        return (title, body);
    }
}
