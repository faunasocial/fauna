namespace FaunaApp.Core.Services;

/// <summary>
/// Pure decision behind windows' home-screen widget — the numeric badge on Fauna's
/// taskbar button (apps/windows.md § Home-screen widget; the cross-app promise is
/// common.md § Home-screen widget). The WinRT call lives in the app project's
/// <c>TaskbarBadgeService</c>; this is the platform-agnostic policy it consults, so
/// it is unit-testable in FaunaApp.Tests with no WinRT types — the
/// <see cref="AutoStartGate"/> / <see cref="SingleInstanceGate"/> pattern.
///
/// <para>The number itself is never computed here: it is the shared
/// <c>ConversationsManager.UnreadTotal()</c> fold (<c>fauna_conversations::sum_unread</c>
/// over every thread), the same number linux paints on its launcher badge, so the
/// widget can never show a count the app would not. This class only decides what to
/// hand the OS for a given total, and whether anything changed since the last push.</para>
/// </summary>
public static class TaskbarBadgePolicy
{
    /// <summary>
    /// The badge-notification XML for <paramref name="unreadTotal"/>, or <c>null</c>
    /// when the badge should be CLEARED. Zero clears rather than painting a "0": a
    /// badge is the glanceable "something is waiting", and an empty inbox shows
    /// nothing, exactly as linux's <c>count-visible</c> goes false at zero. Values
    /// above 99 are passed through unclamped — the OS owns the glyph and renders
    /// them as "99+" itself (badge notifications, numeric badges).
    /// </summary>
    public static string? Payload(uint unreadTotal)
        => unreadTotal == 0 ? null : $"<badge value=\"{unreadTotal}\"/>";
}

/// <summary>
/// De-duplicates badge pushes: the conversations manager notifies on EVERY snapshot
/// change (a draft keystroke, a selection, a read), and only a changed total is worth
/// a WinRT round trip. The first observation always pushes — even a zero — so a badge
/// left behind by a previous run of the app (the OS keeps a badge across process
/// exits, which is what makes it a widget) is reconciled to this session's truth.
/// </summary>
public sealed class TaskbarBadgeTracker
{
    private uint? _lastPushed;
    private readonly object _lock = new();

    /// <summary>
    /// Whether <paramref name="unreadTotal"/> must be pushed to the OS: true on the
    /// first call and whenever the total differs from the last pushed one; false for
    /// a repeat of the current badge. Records the value as pushed when it returns
    /// true.
    /// </summary>
    public bool ShouldPush(uint unreadTotal)
    {
        lock (_lock)
        {
            if (_lastPushed == unreadTotal) return false;
            _lastPushed = unreadTotal;
            return true;
        }
    }

    /// <summary>The last total handed to the OS, or <c>null</c> before the first push.</summary>
    public uint? LastPushed
    {
        get { lock (_lock) { return _lastPushed; } }
    }
}
