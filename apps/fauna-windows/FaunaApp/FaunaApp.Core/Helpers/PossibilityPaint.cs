using FaunaApp.Core.Services;
using Possibility = uniffi.fauna_core.Possibility;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// What one gated affordance paints from the shared <c>Possibility</c> answer its
/// predicate gave (<c>docs/goal/ui/README.md</c> § Offer only what is possible, rules 2
/// and 6): <i>possible</i> → shown and enabled; <i>blocked</i> → shown, disabled, with
/// <see cref="Reason"/> (already resolved through <see cref="Strings.Resolve"/>) painted
/// within eyeshot; <i>absent</i> → not shown at all. The windows app's one reading of the
/// answer — every page paints from this, never from the raw variant, and the action asks
/// the same answer before it runs.
///
/// <para>Lives in <c>FaunaApp.Core</c> so view models and their tests read it without
/// XAML; the WinUI half that applies it to a control is
/// <c>FaunaApp.Helpers.PossibilityPaintExtensions</c>.</para>
/// </summary>
internal readonly record struct PossibilityPaint(bool Shown, bool Enabled, string? Reason)
{
    /// <summary>Whether the affordance may run now: shown and enabled.</summary>
    internal bool IsPossible => Shown && Enabled;

    /// <summary>The paint for <paramref name="answer"/>.</summary>
    internal static PossibilityPaint Of(Possibility answer) => answer switch
    {
        Possibility.Possible => new PossibilityPaint(Shown: true, Enabled: true, Reason: null),
        Possibility.Blocked blocked => new PossibilityPaint(Shown: true, Enabled: false, Reason: Strings.Resolve(blocked.@reason)),
        _ => new PossibilityPaint(Shown: false, Enabled: false, Reason: null),
    };
}
