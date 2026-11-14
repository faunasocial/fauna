#if DEBUG || FAUNA_E2E_AGENT
using System;
using System.Collections.Generic;
using System.Text;
using System.Threading;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;

namespace FaunaApp.Testing;

/// <summary>
/// The windows leg of the <c>painted_text_area</c> state field — convention 17's
/// <c>painted-text-has-area</c> invariant
/// (<c>tests/e2e-unified/helpers/frame_invariants.py::_check_painted_text_has_area</c>,
/// which owns the field's contract and the verdict).
///
/// <para><b>The bug class.</b> A <c>*</c> column gets only what the <c>Auto</c>
/// columns beside it leave, so a narrow pane can give a title 0 px: UIA reports it
/// offscreen, a driver reads it NOT VISIBLE, and a human sees no title at all. The
/// thread header shipped it three times (<c>Controls/ThreadHeader.xaml</c>'s
/// comments), each found by a hand-written test that happened to read the starved
/// element. This reader lets every e2e frame ask the question of every element at
/// once.</para>
///
/// <para><b>Why the app reads it, not the bridge.</b> The FlaUI bridge's
/// <c>/registry</c> frame drops every <c>IsOffscreen</c> element, so a starved one
/// is simply absent from it, and UIA's <c>IsOffscreen</c> reads a starved element
/// and one merely scrolled out of view the same. Only the layout tells them apart,
/// and only the app has it.</para>
///
/// <para><b>⚠ The measure is the ROOM the layout gave, never <c>ActualWidth</c>.</b>
/// An element arranged into a slot smaller than its desired size keeps its desired
/// size and is clipped to the slot (WinUI's layout clip): the starved title's
/// <c>ActualWidth</c> stays its text's width while a human sees none of it. Measured
/// — the first red-verification of this reader read <c>ActualWidth</c>, saw the
/// reverted header's title as laid out, and reported nothing while UIA read
/// <c>thread-header</c> at <c>-74,-74,0,0</c>. So an element's box here is its
/// actual size capped by its layout slot (<see cref="LayoutInformation.GetLayoutSlot"/>,
/// less its margin), and capped again by every ancestor that was itself clipped to
/// its slot — a starved wrapper starves everything inside it. A <c>ScrollViewer</c>'s
/// viewport is NOT a layout clip: its content is arranged at full extent, so an
/// element scrolled out of view keeps its box, which is the one discrimination
/// <c>IsOffscreen</c> cannot make. Positions are ignored: a box is only ever made
/// smaller, so this can miss a starved element, never invent one.</para>
///
/// <para><b>What is published.</b> Every element reached through <c>Visible</c>
/// ancestors only, carrying an explicit automation id and non-empty text of its own
/// (a <c>TextBlock</c>'s or <c>TextBox</c>'s text, or a content control's string
/// content, icon-font glyphs dropped), whose box is under one DIP in either dimension
/// — with the box of its nearest ancestor that HAS one (the pane it was starved in;
/// null when none has) and how many box-less ancestors lie between. The checker, not
/// this reader, decides what is a violation: an entry whose pane has no area is a
/// frame that shows nothing, not a starved element.</para>
///
/// <para><b>One observation per layout pass</b>, scheduled exactly as
/// <see cref="PaintedErrorObserver"/> schedules its own: <c>LayoutUpdated</c> fires
/// after every pass anywhere in the tree, and one High-priority dispatch per burst
/// reads the laid-out frame before the dispatcher runs anything that could change it
/// again. The walk only reads layout — it invalidates nothing, so it can never cause
/// the pass it observes. The snapshot is built on the UI thread and swapped in whole;
/// the state serializer, on the agent's poll thread, only reads the reference
/// (convention 11: the state provider does no UI access).</para>
///
/// <para>Test builds only (convention 15): its one caller is the agent start-up
/// block in <c>App</c>, itself compiled only under this same gate.</para>
/// </summary>
internal static class PaintedTextAreaObserver
{
    /// <summary>The checker's own threshold: a box under one DIP shows nothing.</summary>
    private const double MinReadableDip = 1.0;

    /// <summary>At most this many entries per frame — a build broken in a shared way
    /// starves every frame, and the state push is the command ack path.</summary>
    private const int MaxEntries = 16;

    /// <summary>Layout rounding moves an arranged size by a fraction of a pixel; only
    /// an overflow past that is a layout clip.</summary>
    private const double ClipSlackDip = 0.5;

    private static bool _installed;
    private static bool _queued;
    private static Window? _window;
    private static DispatcherQueue? _dispatcher;
    private static IReadOnlyList<Dictionary<string, object?>> _starved =
        Array.Empty<Dictionary<string, object?>>();
    private static string _lastShown = "";

    /// <summary>What the walk carries down: the room every clipped ancestor left, and
    /// the nearest ancestor with a box of its own.</summary>
    private readonly record struct Context(double ClipW, double ClipH, double PaneW, double PaneH,
        bool HasPane, int Depth);

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

    /// <summary>The <c>painted_text_area</c> state value: the last laid-out frame's
    /// starved text elements. A field read only — safe from any thread.</summary>
    internal static Dictionary<string, object?> State() => new()
    {
        ["starved"] = Volatile.Read(ref _starved),
    };

    private static void Schedule()
    {
        if (_queued || _dispatcher is null) return;
        _queued = true;
        if (!_dispatcher.TryEnqueue(DispatcherQueuePriority.High, ObserveFrame))
            _queued = false;
    }

    // The walk's own cost, traced (agent log only): it runs on the UI thread after
    // every layout burst, so a slow one competes with the input and UIA calls a
    // test is waiting on. Measured 2026-11-10 on the feed: mean 2.2-2.5 ms over ~600
    // elements, about 25 walks in a whole test; the first walk (JIT) about 20 ms.
    private const double SlowWalkMs = 16;
    private static int _walks;
    private static long _walkTicks;
    private static long _walkMaxTicks;
    private static int _visited;

    private static void ObserveFrame()
    {
        _queued = false;
        if (_window?.Content is not UIElement content) return;
        var started = System.Diagnostics.Stopwatch.GetTimestamp();
        _visited = 0;
        var starved = new List<Dictionary<string, object?>>();
        var top = new Context(double.PositiveInfinity, double.PositiveInfinity, 0, 0, false, 0);
        Walk(content, top, starved);
        if (content.XamlRoot is { } xamlRoot)
        {
            // ContentDialogs and flyouts paint from popup roots outside the
            // window content's tree; each is its own root, with its own room.
            foreach (var popup in VisualTreeHelper.GetOpenPopupsForXamlRoot(xamlRoot))
            {
                if (popup.Child is UIElement child) Walk(child, top, starved);
            }
        }
        Volatile.Write(ref _starved, starved);
        if (Core.Logs.E2eTrace.Enabled)
        {
            var ticks = System.Diagnostics.Stopwatch.GetTimestamp() - started;
            _walks++;
            _walkTicks += ticks;
            _walkMaxTicks = Math.Max(_walkMaxTicks, ticks);
            var f = (double)System.Diagnostics.Stopwatch.Frequency / 1000;
            if (_walks % 100 == 0 || ticks / f >= SlowWalkMs)
            {
                Core.Logs.E2eTrace.Write(
                    $"[painted-text-area] cost: {_walks} walks, mean {_walkTicks / f / _walks:F2} ms, " +
                    $"max {_walkMaxTicks / f:F2} ms, this {ticks / f:F2} ms over {_visited} elements");
            }
        }
        // One trace line per change of what is starved, never per frame.
        var shown = string.Join(" | ", starved.ConvertAll(Describe));
        if (shown != _lastShown)
        {
            _lastShown = shown;
            Core.Logs.E2eTrace.Write($"[painted-text-area] {(shown.Length == 0 ? "(none)" : shown)}");
        }
    }

    private static void Walk(UIElement element, Context ctx, List<Dictionary<string, object?>> starved)
    {
        if (element.Visibility != Visibility.Visible) return;
        _visited++;
        var inner = ctx;
        if (element is FrameworkElement fe)
        {
            // The room the parent's arrangement gave: the slot, less the margin.
            var slot = LayoutInformation.GetLayoutSlot(fe);
            var margin = fe.Margin;
            var roomW = Math.Max(0, slot.Width - margin.Left - margin.Right);
            var roomH = Math.Max(0, slot.Height - margin.Top - margin.Bottom);
            // The box a human is given: the arranged size, capped by that room and by
            // every clipped ancestor's.
            var boxW = Math.Min(Math.Min(fe.ActualWidth, roomW), ctx.ClipW);
            var boxH = Math.Min(Math.Min(fe.ActualHeight, roomH), ctx.ClipH);

            if (starved.Count < MaxEntries && !HasArea(boxW, boxH))
            {
                var id = AutomationProperties.GetAutomationId(fe);
                if (!string.IsNullOrEmpty(id))
                {
                    var text = OwnText(fe);
                    if (text.Length > 0)
                    {
                        starved.Add(new Dictionary<string, object?>
                        {
                            ["id"] = id,
                            ["text"] = text,
                            ["width"] = Math.Round(boxW, 1),
                            ["height"] = Math.Round(boxH, 1),
                            ["pane"] = ctx.HasPane
                                ? new Dictionary<string, object?>
                                {
                                    ["width"] = Math.Round(ctx.PaneW, 1),
                                    ["height"] = Math.Round(ctx.PaneH, 1),
                                }
                                : null,
                            ["depth"] = ctx.Depth,
                        });
                    }
                }
            }

            // An element arranged larger than its room is clipped to it, and its
            // descendants with it.
            var clipW = fe.ActualWidth > roomW + ClipSlackDip ? Math.Min(ctx.ClipW, roomW) : ctx.ClipW;
            var clipH = fe.ActualHeight > roomH + ClipSlackDip ? Math.Min(ctx.ClipH, roomH) : ctx.ClipH;
            inner = HasArea(boxW, boxH)
                ? new Context(clipW, clipH, boxW, boxH, true, 0)
                : ctx with { ClipW = clipW, ClipH = clipH, Depth = ctx.Depth + 1 };
        }
        var count = VisualTreeHelper.GetChildrenCount(element);
        for (var i = 0; i < count; i++)
        {
            if (VisualTreeHelper.GetChild(element, i) is UIElement child) Walk(child, inner, starved);
        }
    }

    private static bool HasArea(double width, double height) =>
        width >= MinReadableDip && height >= MinReadableDip;

    /// <summary>The element's OWN text — never a descendant's, which carries its
    /// own box and is judged on its own.</summary>
    private static string OwnText(FrameworkElement fe)
    {
        var raw = fe switch
        {
            TextBlock tb => tb.Text,
            TextBox box => box.Text,
            ContentControl { Content: string s } => s,
            _ => null,
        };
        if (string.IsNullOrEmpty(raw)) return "";
        var sb = new StringBuilder(raw.Length);
        foreach (var c in raw)
        {
            // Segoe icon glyphs (a gear button's content) are Private Use Area
            // code points — not text a user reads.
            if (c is >= '' and <= '') continue;
            sb.Append(c);
        }
        return sb.ToString().Trim();
    }

    private static string Describe(Dictionary<string, object?> e)
    {
        var pane = e["pane"] is Dictionary<string, object?> p ? $"{p["width"]}x{p["height"]}" : "none";
        return $"{e["id"]} {e["width"]}x{e["height"]} pane={pane} depth={e["depth"]}";
    }
}
#endif
