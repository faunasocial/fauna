namespace FaunaApp.Core.Helpers;

/// <summary>
/// Watches how much of the UI thread a page's binding re-evaluation is eating, and
/// reports only when it is pathological.
///
/// <para>WHY THIS EXISTS. Every windows page that binds a UniFFI machine follows one
/// pattern: the machine calls the app's observer from a Tokio thread, the observer
/// marshals to the UI thread and raises <c>PropertyChanged</c> with an EMPTY property
/// name, and WinUI re-evaluates every binding on the page. Each <c>OneWay</c> source
/// is an FFI call that takes the machine's state lock, so the cost of one tick is the
/// size of the page — and the cost per second is that times the notification rate.</para>
///
/// <para>When that product approaches 100% of the thread, the app does not look
/// broken: it looks fine to a human, and it becomes <b>undrivable</b>. UI Automation
/// is served by the same thread, so what a saturated UI thread produces is
/// <c>ElementFromHandle</c> timing out — a window resolve that costs 21 ms on a quiet
/// page was measured at 22.2 s on <c>vps_config</c>, and every element route
/// re-resolves the window before it searches.</para>
///
/// <para>TWO FAILURE SHAPES, AND WHY BOTH ARE CHECKED. A tick-rate threshold alone
/// sees only a notifying machine in a tight loop. The other shape is FEW but
/// EXPENSIVE re-evaluations — a binding source that blocks, e.g. an FFI getter
/// waiting on the machine's state lock while a long call holds it — which pins the
/// thread just as hard while the tick count stays low. Reporting on the busy SHARE as
/// well as the rate is what makes the second shape visible; a detector that missed it
/// would have called a page spending 100% of its thread in four blocking reads
/// healthy.</para>
///
/// <para>This type is the pure decision, kept out of the WinUI observers so it can be
/// tested without a dispatcher (<c>UiThreadBudgetTests</c>) and shared by all of them
/// — the same observer shape appears about ten times across feed, conversations,
/// search, devices, folders and onboarding. Callers supply their own monotonic clock
/// reading, so there is no hidden time source and no wall-clock flakiness.</para>
/// </summary>
public sealed class UiThreadBudget
{
    /// <summary>Re-evaluations within one window above which the machine is
    /// notifying faster than the page can be repainted.</summary>
    public const int DefaultStormTicksPerWindow = 20;

    /// <summary>How long a measurement window is. Long enough that a burst (a page
    /// arriving, a catalog landing) does not trip it; short enough to name the phase
    /// of the journey it happened in.</summary>
    public const double DefaultWindowSeconds = 2.0;

    /// <summary>Share of the window spent inside binding re-evaluation above which
    /// the thread is effectively unavailable to anything else — UIA included.</summary>
    public const double DefaultBusyShare = 0.5;

    private readonly int _stormTicks;
    private readonly double _windowSeconds;
    private readonly double _busyShare;

    private int _ran;
    private double _invokeMs;
    private double _windowStart = double.NaN;

    public UiThreadBudget(
        int stormTicksPerWindow = DefaultStormTicksPerWindow,
        double windowSeconds = DefaultWindowSeconds,
        double busyShare = DefaultBusyShare)
    {
        _stormTicks = stormTicksPerWindow;
        _windowSeconds = windowSeconds;
        _busyShare = busyShare;
    }

    /// <summary>What a closed, pathological window looked like.</summary>
    /// <param name="Ran">Full-page re-evaluations in the window.</param>
    /// <param name="WindowSeconds">The window's real length.</param>
    /// <param name="InvokeMs">Time inside those re-evaluations.</param>
    /// <param name="Storming">The machine notified faster than the page repaints.</param>
    /// <param name="Blocked">The re-evaluations themselves held the thread.</param>
    public readonly record struct Report(
        int Ran, double WindowSeconds, double InvokeMs, bool Storming, bool Blocked)
    {
        /// <summary>Share of the window spent inside binding re-evaluation, 0..1.</summary>
        public double BusyShare => WindowSeconds > 0 ? InvokeMs / (WindowSeconds * 1000.0) : 0;

        /// <summary>One line, naming which shape it is and what it costs. Says what
        /// the number MEANS for the reader who hit this as a UIA timeout, because
        /// that is the symptom this is nearly always read from.</summary>
        public override string ToString()
            => $"[fauna] UI thread {(Storming ? "STORM" : "BLOCKED")}: {Ran} full-page binding "
             + $"re-evaluations in {WindowSeconds:N1}s ({Ran / WindowSeconds:N0}/s), "
             + $"{InvokeMs:N0}ms of that window spent inside them "
             + $"({BusyShare * 100.0:N0}% of the UI thread). Every UIA call against this "
             + "app is served by the same thread, so this is what an ElementFromHandle "
             + "timeout looks like from the inside.";
    }

    /// <summary>
    /// Record one completed re-evaluation. Returns a <see cref="Report"/> when a
    /// window just closed AND it was pathological; <c>null</c> otherwise — so a
    /// healthy page produces no output at all, and the caller has nothing to
    /// rate-limit itself.
    /// </summary>
    /// <param name="nowSeconds">A monotonic clock reading, in seconds.</param>
    /// <param name="invokeMs">Time this re-evaluation took, in milliseconds.</param>
    public Report? Record(double nowSeconds, double invokeMs)
    {
        _ran++;
        _invokeMs += invokeMs;

        // The first call only starts the window: with no previous reading there is no
        // elapsed time to judge, and treating the epoch as zero would report an
        // instant 0 s window as infinitely busy.
        if (double.IsNaN(_windowStart))
        {
            _windowStart = nowSeconds;
            return null;
        }

        var windowSeconds = nowSeconds - _windowStart;
        if (windowSeconds < _windowSeconds) return null;

        var storming = _ran >= _stormTicks;
        var blocked = _invokeMs >= windowSeconds * 1000.0 * _busyShare;
        Report? report = storming || blocked
            ? new Report(_ran, windowSeconds, _invokeMs, storming, blocked)
            : null;

        _ran = 0;
        _invokeMs = 0;
        _windowStart = nowSeconds;
        return report;
    }
}
