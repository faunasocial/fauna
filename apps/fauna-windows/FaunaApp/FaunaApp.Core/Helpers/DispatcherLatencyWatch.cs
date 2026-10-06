namespace FaunaApp.Core.Helpers;

/// <summary>
/// Measures whether the UI thread is actually pumping, independently of any theory
/// about what might be holding it.
///
/// <para>WHY THIS EXISTS, AND WHY IT IS NOT THE SAME QUESTION AS
/// <see cref="UiThreadBudget"/>. When UI Automation cannot resolve the app's window
/// (<c>ElementFromHandle</c> timing out), the standard reading is "the app's UI thread
/// is not servicing the window message UIA sends". That reading is an INFERENCE, and
/// on `vps_config` it is the premise every remaining lead was built on — while the one thing actually measured
/// there says the thread is NOT busy re-evaluating bindings, which was the leading
/// candidate for what would be holding it.</para>
///
/// <para>So the inference needs its own witness. A caller ticks this from a timer ON
/// the UI thread at a known interval and reports its own lateness: if the ticks arrive
/// on time while UIA times out, the thread is pumping and the problem is not
/// starvation at all — which would redirect the whole investigation. If the ticks are
/// late by the same order as the UIA timeouts, starvation is confirmed and the
/// remaining question is only who. Either answer is worth more than another run spent
/// theorising, and no other instrument can give it: everything else in this app is
/// observed THROUGH the automation surface whose health is the thing in doubt.</para>
///
/// <para>Silent unless late, like <see cref="UiThreadBudget"/>: a healthy app writes
/// nothing at all, so this can stay in place without becoming noise.</para>
/// </summary>
public sealed class DispatcherLatencyWatch
{
    /// <summary>Lateness above which a tick is worth reporting. Well clear of
    /// ordinary scheduling jitter and of a frame or two of real work, and far below
    /// the ~2 s UIA provider timeout this exists to be compared against.</summary>
    public const double DefaultLateMs = 500.0;

    private readonly double _expectedIntervalMs;
    private readonly double _lateMs;
    private double _lastTick = double.NaN;

    /// <param name="expectedIntervalMs">The interval the caller's timer is set to.</param>
    /// <param name="lateMs">Lateness beyond the interval that counts as a stall.</param>
    public DispatcherLatencyWatch(double expectedIntervalMs, double lateMs = DefaultLateMs)
    {
        _expectedIntervalMs = expectedIntervalMs;
        _lateMs = lateMs;
    }

    /// <summary>A stalled tick: how long the UI thread went without running one.</summary>
    /// <param name="GapMs">Real time since the previous tick.</param>
    /// <param name="ExpectedMs">What that gap should have been.</param>
    public readonly record struct Stall(double GapMs, double ExpectedMs)
    {
        /// <summary>How much longer the gap was than it should have been.</summary>
        public double LateByMs => GapMs - ExpectedMs;

        public override string ToString()
            => $"[fauna] UI thread STALLED: a {ExpectedMs:N0}ms timer tick arrived "
             + $"{LateByMs:N0}ms late (gap {GapMs:N0}ms). The thread was not running "
             + "queued work for that long, so anything served by it — every UI "
             + "Automation call against this app included — was unanswerable "
             + "meanwhile. If UIA is timing out and this line is ABSENT, the thread "
             + "is pumping and the fault is not starvation.";
    }

    /// <summary>
    /// Record a timer tick. Returns a <see cref="Stall"/> when this tick arrived
    /// late enough to matter; <c>null</c> otherwise.
    /// </summary>
    /// <param name="nowMs">A monotonic clock reading, in milliseconds.</param>
    public Stall? Tick(double nowMs)
    {
        if (double.IsNaN(_lastTick))
        {
            // Nothing to compare the first tick against; the interval before it
            // includes the timer's own start-up and would report a false stall.
            _lastTick = nowMs;
            return null;
        }

        var gap = nowMs - _lastTick;
        _lastTick = nowMs;
        return gap > _expectedIntervalMs + _lateMs ? new Stall(gap, _expectedIntervalMs) : null;
    }
}
