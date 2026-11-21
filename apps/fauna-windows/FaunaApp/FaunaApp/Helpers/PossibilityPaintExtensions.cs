using FaunaApp.Core.Helpers;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace FaunaApp.Helpers;

/// <summary>
/// The WinUI half of <see cref="PossibilityPaint"/> (<c>docs/goal/ui/README.md</c>
/// § Offer only what is possible): applies one shared answer to the control it gates —
/// absent → <see cref="Visibility.Collapsed"/> (no UIA peer, so the e2e counts 0);
/// possible → visible and enabled; blocked → visible, disabled, the reason on the
/// control's tooltip and, where the page has no line already stating it, on the
/// <paramref name="reasonLine"/> the caller names (the windows inline-caption idiom,
/// never a global banner). A page whose own line already shows the same sentence
/// (the logs empty line, the rule editor's status, the room notice) passes no
/// <paramref name="reasonLine"/>, so the reason is never painted twice.
///
/// <para>The reason it paints is also the control's UIA <c>FullDescription</c>
/// (empty while possible): what a screen reader reads beside the disabled control,
/// and what the e2e bridge answers as the element's <c>reason</c> attribute
/// (<c>drivers/base.py::painted_reason</c>). <c>FullDescription</c> and not HelpText,
/// because HelpText is already the app's free-text automation channel on many gated
/// controls (a copy button's confirmation, a compose field's visible text), so a
/// reason written there would be overwritten or overwrite them.</para>
/// </summary>
internal static class PossibilityPaintExtensions
{
    internal static void ApplyPossibility(this Control control, PossibilityPaint paint, TextBlock? reasonLine = null)
    {
        control.Visibility = paint.Shown ? Visibility.Visible : Visibility.Collapsed;
        control.IsEnabled = paint.Enabled;
        ToolTipService.SetToolTip(control, paint.Reason);
        AutomationProperties.SetFullDescription(control, paint.Reason ?? string.Empty);
        if (reasonLine is null) return;
        reasonLine.Text = paint.Reason ?? string.Empty;
        reasonLine.Visibility = string.IsNullOrEmpty(paint.Reason) ? Visibility.Collapsed : Visibility.Visible;
    }
}
