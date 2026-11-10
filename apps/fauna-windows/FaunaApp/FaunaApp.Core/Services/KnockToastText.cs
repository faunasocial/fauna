using uniffi.fauna_core;
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
    ///
    /// <para><c>BodyKey</c> is the catalog key the body resolved from (<c>null</c>
    /// for a verbatim body) — what the knock-toast log records as <c>key</c>
    /// (<c>fauna_e2e_agent::KNOCK_NOTICES_KEY</c>).</para>
    /// </summary>
    public static (string Title, string Body, string? BodyKey) For(FfiKnock knock)
    {
        var title = Strings.Get("notifications/knock_title");
        var (body, key) = FaunaFfiMethods.KnockTextFor(knock) switch
        {
            FfiNotificationText.Localized l => (Strings.Resolve(l.text), l.text.@key),
            FfiNotificationText.Verbatim v => (v.text, (string?)null),
            _ => (Strings.Get("notifications/knock_body"), "notifications.knock_body"),
        };
        return (title, body, key);
    }

    /// <summary>
    /// Whether this knock raises no toast: its message carries a word muted at
    /// Hide, so the row it rings collapses on the notifications page, and a toast
    /// cannot be revealed (`notifications.md` § Muted words rule 6). The shared
    /// <c>knock_push_muted_for</c> decides; <paramref name="keywords"/> is the
    /// process-held list (<see cref="MutedKeywordsCache.Keywords"/>), never a
    /// fresh read.
    /// </summary>
    public static bool Withheld(FfiKnock knock, IReadOnlyList<MutedKeyword> keywords) =>
        FaunaFfiMethods.KnockPushMutedFor(knock, keywords as MutedKeyword[] ?? keywords.ToArray());
}
