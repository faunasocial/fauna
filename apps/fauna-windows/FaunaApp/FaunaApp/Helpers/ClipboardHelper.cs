using Windows.ApplicationModel.DataTransfer;

namespace FaunaApp.Helpers;

/// <summary>
/// One shared WinUI copy-to-clipboard primitive. Replaces the per-page
/// hand-rolled <c>new DataPackage(); dp.SetText(x); Clipboard.SetContent(dp)</c>
/// idiom (priority #2/#3/#4 — shared platform-family code, same concept
/// everywhere). The iOS <c>FaunaKit.CopyButton</c> and android
/// <c>CopyButton.kt</c> are the cross-app prior art.
///
/// Deliberately does NOT swallow exceptions: callers that want to surface or
/// ignore clipboard contention keep their own try/catch around the call (e.g.
/// AdminDns surfaces via ShowError; AdminUsers swallows silently). This keeps
/// each site's existing error semantics intact.
/// </summary>
internal static class ClipboardHelper
{
    /// <summary>Copy <paramref name="text"/> to the system clipboard. Null or
    /// empty input is a no-op (the dominant guard across the prior sites).</summary>
    public static void CopyText(string? text)
    {
        if (string.IsNullOrEmpty(text)) return;
        var dp = new DataPackage();
        dp.SetText(text);
        Clipboard.SetContent(dp);
    }
}
