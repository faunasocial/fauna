namespace FauiBridge;

/// <summary>
/// A gesture on an element that carries none is refused "not actuable", never
/// struck as pixels or typed into whatever else holds focus
/// (<c>docs/goal/architecture/e2e-conventions.md</c> § convention 11 — the
/// structural refusal tui, linux and apple already answer; the inert-gesture
/// vocabulary every journey reads through <c>helpers/inert_refusal.py</c>).
///
/// <para><b>Why windows needed it.</b> tui, linux and apple drive a click as the
/// element's own activation and refuse when it has none — linux: <i>"element is
/// not activatable and has no click gesture"</i>, so a reply of <c>ok</c> never
/// covers a click that ran nothing. This bridge drives through UIA and, on an
/// element exposing no pattern, fell back to a physical <c>SendInput</c> click (and,
/// for a key, injected it into whatever held focus). On an inert element that
/// fallback either acked a click that ran nothing or, on a box whose automation
/// session is disconnected (<c>query session</c> → <c>Disc</c>), failed
/// <c>Win32Exception(5)</c> "Access is denied" — the critical-alert witness's
/// "the alarm row cannot be dismissed" check went red on windows for exactly that
/// reason, on a row that is a bare <c>TextBlock</c> no user can activate.</para>
///
/// <para><b>Only what is provably unreachable is refused.</b> UIA cannot see a
/// <c>Tapped</c> handler, so a pattern-less element is not, by itself, inert:
/// <c>dm-attachment-image</c> is an <c>Image</c> whose tap opens the lightbox. The
/// two verdicts below are therefore narrow:</para>
/// <list type="bullet">
/// <item><b>A key</b> is routed from the focused element up through its ancestors.
/// An element that cannot take keyboard focus and holds no descendant that could
/// is never on any key's route, so a key "on" it can reach nothing — a fact of the
/// routing, needing no knowledge of the app.</item>
/// <item><b>A click</b> is refused only on a TEXT label (<c>ControlType.Text</c>)
/// that takes no focus, holds no children (an inline <c>Hyperlink</c> is a child),
/// and has no UIA activation (Invoke, Toggle, SelectionItem, ExpandCollapse) or
/// editable value on itself or any ancestor — the registry's own
/// <c>actuable</c>/<c>editable</c> predicates, so the refusal and
/// <c>/registry</c> cannot disagree. A text inside a Button, a list item or a nav
/// item keeps today's physical click, which the container handles. What UIA cannot
/// see — a pointer handler on a text element or on a panel around one — is held
/// to zero by <c>tests/e2e-unified/tests/test_windows_pointer_gesture_ratchet.py</c>,
/// which enumerates every pointer-gesture handler FaunaApp attaches and fails on a
/// new one.</item>
/// </list>
///
/// <para>Everything here is pure, so <see cref="SelfTest.ActuationGateChecks"/>
/// pins it with no UIA element, no window and no app.</para>
/// </summary>
internal static class InertGesture
{
    /// <summary>The phrase every agent's inert refusal carries — tui's and apple's
    /// wording, and one of the phrases <c>helpers/inert_refusal.py</c> recognises.
    /// A journey that tolerates "there is nothing here to activate" matches on it,
    /// so it is a contract, not prose.</summary>
    public const string Phrase = "not actuable";

    /// <summary>Why a click on a text label is refused.</summary>
    public const string ClickWhy =
        "it is a text label that takes no keyboard focus, holds no children, and "
        + "has no UIA activation (Invoke, Toggle, SelectionItem, ExpandCollapse) or "
        + "editable value on itself or any ancestor, so a click on it reaches no "
        + "control; the bridge refuses rather than striking pixels that run nothing";

    /// <summary>Why a key on a non-focusable leaf is refused.</summary>
    public const string KeyWhy =
        "it takes no keyboard focus and holds nothing that can, so no key press is "
        + "ever routed to it; the bridge refuses rather than sending the key to "
        + "whatever else holds focus";

    /// <summary>Does a click on this element provably reach nothing?
    /// <paramref name="chainActivates"/> is "the element or an ancestor is
    /// actuable or editable", and a caller that could not read it passes
    /// <c>true</c> — an unreadable property is never evidence of inertness.</summary>
    public static bool ClickReachesNothing(
        bool isText, bool keyboardFocusable, bool hasChildren, bool chainActivates)
        => isText && !keyboardFocusable && !hasChildren && !chainActivates;

    /// <summary>Does a key press "on" this element provably reach nothing? A caller
    /// that could not read a property passes the value that keeps the key
    /// (focusable / has children).</summary>
    public static bool KeyReachesNothing(bool keyboardFocusable, bool hasChildren)
        => !keyboardFocusable && !hasChildren;

    /// <summary>The 409 refusal body's <c>error</c> — apple's
    /// <c>"&lt;route&gt;: &lt;id&gt; is not actuable"</c> shape, naming the index and
    /// the reason (convention 6: the failure diagnoses itself).</summary>
    public static string RefusalMessage(string route, string id, int index, string why)
        => $"{route}: \"{id}\"[{index}] is {Phrase} — {why}";
}

/// <summary>A gesture route was asked to act on an element that provably carries
/// no gesture (<see cref="InertGesture"/>). <c>Program</c> answers it 409, the
/// status every other app's agent answers a structural refusal with, so the driver
/// raises it as a <c>RuntimeError</c> naming the element — never a 404, which would
/// send <c>drivers/http_bridge.py</c> into its scroll-retry loop.</summary>
internal sealed class NotActuableException : Exception
{
    public string Route { get; }
    public string Id { get; }
    public int Index { get; }

    public NotActuableException(string route, string id, int index, string why)
        : base(InertGesture.RefusalMessage(route, id, index, why))
    {
        Route = route;
        Id = id;
        Index = index;
    }
}
