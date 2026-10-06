namespace FaunaApp.Core.Helpers;

/// <summary>
/// Presence rule for the windows test-message shims: the global
/// <c>error-message</c>/<c>warning-message</c>/<c>info-message</c> TextBoxes on
/// <c>MainPage.xaml</c> and the per-page 1×1 <c>ErrorTextMirror</c>/
/// <c>WarningTextMirror</c> TextBlocks.
/// <para>
/// <b>A shim is in the accessibility tree iff it carries a message.</b> The shims
/// share their <c>AutomationId</c> with the page's own error InfoBar, and the FlaUI
/// bridge resolves an id window-wide and checks only <c>!IsOffscreen</c> — so an
/// always-on-screen empty shim makes <c>is_visible("error-message")</c> read true on
/// a clean page, and every "no error at page start" assertion becomes unobservable.
/// Collapsing the shim when it has nothing to say is what makes the id honest, and
/// matches what the other apps get for free (linux prunes non-showing widgets in
/// <c>automation/find.rs::is_showing</c>; tui/web build the surface hidden).
/// </para>
/// <para>
/// Whitespace counts as absent: the mirrors' XAML default and historical "cleared"
/// value is a literal <c>" "</c>, so an <c>IsNullOrEmpty</c> test would keep all 24
/// of them on-screen and fix nothing. Pinned in <c>MessageShimTests</c>.
/// </para>
/// </summary>
public static class MessageShim
{
    /// <summary>Whether a shim carrying <paramref name="message"/> should be shown.</summary>
    public static bool ShouldShow(string? message) => !string.IsNullOrWhiteSpace(message);
}
