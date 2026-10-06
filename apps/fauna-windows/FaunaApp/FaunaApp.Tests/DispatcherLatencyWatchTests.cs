using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Pins <see cref="DispatcherLatencyWatch"/>, whose whole value is that its SILENCE
/// is evidence — the reading "UIA timed out and the UI thread never reported a stall,
/// so the thread was pumping" is only worth anything if a stall would in fact have
/// been reported. A detector trusted for its silence has to be tested for its noise.
/// </summary>
public class DispatcherLatencyWatchTests
{
    [Fact]
    public void OnTimeTicks_ReportNothing()
    {
        var watch = new DispatcherLatencyWatch(expectedIntervalMs: 100);

        watch.Tick(0);
        for (var i = 1; i <= 50; i++)
            Assert.Null(watch.Tick(i * 100.0));
    }

    [Fact]
    public void OrdinaryJitter_IsNotAStall()
    {
        var watch = new DispatcherLatencyWatch(expectedIntervalMs: 100);

        watch.Tick(0);
        Assert.Null(watch.Tick(180));    // 80ms late
        Assert.Null(watch.Tick(560));    // 380ms late — still under the threshold
    }

    [Fact]
    public void ALongGap_IsReportedWithItsRealLateness()
    {
        var watch = new DispatcherLatencyWatch(expectedIntervalMs: 100);

        watch.Tick(0);
        var stall = watch.Tick(22_300);   // the vps_config order of magnitude

        Assert.NotNull(stall);
        Assert.Equal(22_300, stall!.Value.GapMs);
        Assert.Equal(22_200, stall.Value.LateByMs);
        Assert.Contains("STALLED", stall.Value.ToString());
    }

    [Fact]
    public void TheFirstTickIsNeverAStall()
    {
        // The interval before the first tick contains the timer's own start-up, so
        // measuring it would report a stall on every launch.
        var watch = new DispatcherLatencyWatch(expectedIntervalMs: 100);
        Assert.Null(watch.Tick(99_999));
    }

    [Fact]
    public void RecoveryIsClean_SoOneStallDoesNotReportForever()
    {
        var watch = new DispatcherLatencyWatch(expectedIntervalMs: 100);

        watch.Tick(0);
        Assert.NotNull(watch.Tick(5_000));
        Assert.Null(watch.Tick(5_100));
        Assert.Null(watch.Tick(5_200));
    }
}
