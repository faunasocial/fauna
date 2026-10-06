using System;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_devices_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <c>device-p2p-participation-toggle</c>'s paint on a <c>device-card</c>
/// (behavior/p2p.md § Per-device participation; ui/devices.md § User actions) —
/// tui's two arms (<c>apps/fauna-tui/src/settings/devices.rs</c>, pinned by its
/// <c>settings::devices::tests</c>) and FaunaKit's <c>DeviceCard</c>, which the
/// WinUI page cannot host in this assembly, so the rule lives in
/// <see cref="DeviceParticipationPaint"/> and is pinned here. End to end:
/// <c>tests/e2e-unified/tests/test_p2p_participation.py</c>.
/// </summary>
public class DeviceParticipationPaintTests
{
    private static DeviceSummary Device(bool? reported, bool offRequested = false) => new(
        "d1", "laptop", "rw", 0, 0, true, false, Array.Empty<DeviceFolderRole>(), null, reported, offRequested);

    [Fact]
    public void OwnRow_PaintsTheLocalReadOverAStaleReport_AndIsAlwaysActionable()
    {
        // The nest still holds an `on` report; the device-local row says off.
        var paint = DeviceParticipationPaint.For(Device(true), own: true, ownParticipation: false);

        Assert.False(paint.Checked);
        Assert.Equal("devices/p2p_participation_own", paint.LabelKey);
        Assert.True(paint.Actionable);
    }

    [Fact]
    public void OwnRow_FallsBackToTheReport_ThenToOn()
    {
        Assert.False(DeviceParticipationPaint.For(Device(false), own: true, ownParticipation: null).Checked);
        Assert.True(DeviceParticipationPaint.For(Device(null), own: true, ownParticipation: null).Checked);
    }

    [Fact]
    public void OwnRow_EvenWithAnOffRequestPending_KeepsTheOwnLabelAndStaysActionable()
    {
        var paint = DeviceParticipationPaint.For(Device(true, offRequested: true), own: true, ownParticipation: true);

        Assert.Equal("devices/p2p_participation_own", paint.LabelKey);
        Assert.True(paint.Actionable);
    }

    [Fact]
    public void SiblingRow_NeverReported_PaintsOnAndSaysUnreported()
    {
        var paint = DeviceParticipationPaint.For(Device(null), own: false, ownParticipation: false);

        Assert.True(paint.Checked);
        Assert.Equal("devices/p2p_participation_unreported", paint.LabelKey);
        Assert.True(paint.Actionable);
    }

    [Fact]
    public void SiblingRow_ReportedOn_CanBeAskedOff()
    {
        var paint = DeviceParticipationPaint.For(Device(true), own: false, ownParticipation: null);

        Assert.True(paint.Checked);
        Assert.Equal("devices/p2p_participation", paint.LabelKey);
        Assert.True(paint.Actionable);
    }

    [Fact]
    public void SiblingRow_ReportedOff_IsInert_BecauseEnablingIsThatDevicesOwnConsent()
    {
        var paint = DeviceParticipationPaint.For(Device(false), own: false, ownParticipation: true);

        Assert.False(paint.Checked);
        Assert.Equal("devices/p2p_participation", paint.LabelKey);
        Assert.False(paint.Actionable);
    }

    [Fact]
    public void SiblingRow_OffRequestPending_SaysTurningOff_AndIsInert()
    {
        var paint = DeviceParticipationPaint.For(Device(true, offRequested: true), own: false, ownParticipation: null);

        Assert.True(paint.Checked);
        Assert.Equal("devices/p2p_participation_off_requested", paint.LabelKey);
        Assert.False(paint.Actionable);
    }
}
