#if DEBUG || FAUNA_E2E_AGENT
using System.Collections.Generic;
using System.Text;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace FaunaApp.Testing;

/// <summary>
/// The windows leg of <c>fauna_e2e_agent::PAINTED_ERRORS_KEY</c> — every error
/// surface a painted frame showed, counted by shared Rust
/// (<see cref="Core.Services.E2eLoudSurfaces"/>), so "a passing connection gap
/// raises no error anywhere" is the counter not moving, rather than a read after
/// the gap that would miss an error raised and cleared inside it.
///
/// <para><b>One observation per layout pass.</b> Web's twin
/// (<c>apps/fauna-web/src/lib/e2e-painted-errors.ts</c>) marks the page dirty on
/// a DOM mutation and reads the frame in the next <c>requestAnimationFrame</c>,
/// just before it paints. WinUI has no mutation observer, but every change that
/// can show, hide or re-word an error — a <c>Visibility</c> flip, an
/// <c>InfoBar.IsOpen</c> change, new text — invalidates layout, and a layout pass
/// precedes the frame that paints it. <c>LayoutUpdated</c> fires after every pass
/// anywhere in the tree; one High-priority dispatch per burst then reads the
/// frame once, before the dispatcher runs anything that could change it again.</para>
///
/// <para><b>Painted</b> means what the FlaUI bridge's <c>/registry</c> frame keeps,
/// read from the app's side: reached through <c>Visible</c> ancestors only, and
/// laid out with a non-empty box — a closed <c>InfoBar</c> keeps its element but
/// collapses its content to zero height. The text is the element's own visible
/// <c>TextBlock</c> text (an <c>InfoBar</c>'s title and message live in its
/// template's TextBlocks), with icon-font glyphs dropped. The tally applies the
/// contract's error-surface predicate and drops empty text itself.</para>
///
/// <para>Test builds only (convention 15): its one caller is the agent start-up
/// block in <c>App</c>, itself compiled only under this same gate.</para>
/// </summary>
internal static class PaintedErrorObserver
{
    private static bool _installed;
    private static bool _queued;
    private static Window? _window;
    private static DispatcherQueue? _dispatcher;

    /// <summary>Start observing <paramref name="window"/>. Idempotent; call from
    /// the UI thread once the root element exists.</summary>
    internal static void Install(Window window, FrameworkElement root)
    {
        if (_installed) return;
        _installed = true;
        _window = window;
        _dispatcher = window.DispatcherQueue;
        root.LayoutUpdated += (_, _) => Schedule();
        Schedule();
    }

    private static void Schedule()
    {
        if (_queued || _dispatcher is null) return;
        _queued = true;
        if (!_dispatcher.TryEnqueue(DispatcherQueuePriority.High, ObserveFrame))
            _queued = false;
    }

    private static void ObserveFrame()
    {
        _queued = false;
        if (_window?.Content is not UIElement content) return;
        var frame = new List<(string, string)>();
        Walk(content, frame);
        if (content.XamlRoot is { } xamlRoot)
        {
            // ContentDialogs and flyouts paint from popup roots outside the
            // window content's tree.
            foreach (var popup in VisualTreeHelper.GetOpenPopupsForXamlRoot(xamlRoot))
            {
                if (popup.Child is UIElement child) Walk(child, frame);
            }
        }
        Core.Services.E2eLoudSurfaces.ObservePaintedFrame(frame);
        // One trace line per change of what is painted, never per frame: the
        // evidence a count that did (or did not) move can be checked against.
        var shown = string.Join(" | ", frame.ConvertAll(e => $"{e.Item1}={e.Item2}"));
        if (shown != _lastShown)
        {
            _lastShown = shown;
            Core.Logs.E2eTrace.Write($"[painted-errors] {(shown.Length == 0 ? "(none)" : shown)}");
        }
    }

    private static string _lastShown = "";

    private static void Walk(UIElement element, List<(string, string)> frame)
    {
        if (element.Visibility != Visibility.Visible) return;
        if (element is FrameworkElement fe)
        {
            var id = AutomationProperties.GetAutomationId(fe);
            if (!string.IsNullOrEmpty(id) && IsErrorSurface(id))
            {
                if (fe.ActualWidth > 0 && fe.ActualHeight > 0)
                {
                    frame.Add((id, VisibleText(fe)));
                }
                // An error surface's own content is its text, not further surfaces.
                return;
            }
        }
        var count = VisualTreeHelper.GetChildrenCount(element);
        for (var i = 0; i < count; i++)
        {
            if (VisualTreeHelper.GetChild(element, i) is UIElement child) Walk(child, frame);
        }
    }

    /// <summary>A pre-filter only — <c>fauna_e2e_agent::is_error_surface</c>, which
    /// the shared tally re-applies, owns the predicate.</summary>
    private static bool IsErrorSurface(string id) =>
        id == "error-message" || id.EndsWith("-error", System.StringComparison.Ordinal);

    private static string VisibleText(UIElement root)
    {
        var sb = new StringBuilder();
        AppendText(root, sb);
        return sb.ToString().Trim();
    }

    private static void AppendText(UIElement element, StringBuilder sb)
    {
        if (element.Visibility != Visibility.Visible) return;
        // A TextBox (MainPage's global message shims) renders its text through an
        // internal view, not a TextBlock, so its own Text is the read.
        var text = element switch
        {
            TextBlock tb => tb.Text,
            TextBox box => box.Text,
            _ => null,
        };
        if (!string.IsNullOrEmpty(text))
        {
            foreach (var c in text)
            {
                // Segoe icon glyphs (the copy button, the InfoBar severity icon)
                // are Private Use Area code points — not text a user reads.
                if (c is >= '' and <= '') continue;
                sb.Append(c);
            }
            sb.Append(' ');
            if (element is TextBox) return;
        }
        var count = VisualTreeHelper.GetChildrenCount(element);
        for (var i = 0; i < count; i++)
        {
            if (VisualTreeHelper.GetChild(element, i) is UIElement child) AppendText(child, sb);
        }
    }
}
#endif
