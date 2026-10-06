using uniffi.fauna_devices_machine;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// What <c>device-p2p-participation-toggle</c> paints on one <c>device-card</c>
/// (behavior/p2p.md § Per-device participation; ui/devices.md § User actions):
/// whether the box is <see cref="Checked"/>, the i18n key of its label, and
/// whether a click may reach the machine. tui's two arms
/// (<c>apps/fauna-tui/src/settings/devices.rs</c>) and FaunaKit's
/// <c>DeviceCard</c>, rule for rule:
/// <list type="bullet">
/// <item>THIS device's own row (the row <c>device-this-mark-badge</c> marks)
/// paints the device-local read, <c>DevicesSnapshot.ownP2pParticipation</c>,
/// falling back to the row's own report, then on — and is always actionable:
/// the switch is the device's own, both directions.</item>
/// <item>Any other row paints that device's report (never reported → on, and
/// says so), says "turning off" while an off request is pending, and is
/// actionable only while it may be on with no request pending — enabling is
/// local consent on that device, so the machine refuses a remote <c>on</c>
/// too.</item>
/// </list>
/// The gesture is <c>DevicesMachine.SetP2pParticipation(index, !Checked)</c>;
/// which arm it takes is the machine's decision, not this paint's.
/// </summary>
internal readonly record struct DeviceParticipationPaint(bool Checked, string LabelKey, bool Actionable)
{
    public static DeviceParticipationPaint For(DeviceSummary device, bool own, bool? ownParticipation)
    {
        var reported = device.@p2pParticipation;
        if (own)
            return new(ownParticipation ?? reported ?? true, "devices/p2p_participation_own", true);

        var isChecked = reported ?? true;
        var labelKey = device.@p2pOffRequested
            ? "devices/p2p_participation_off_requested"
            : reported is null
                ? "devices/p2p_participation_unreported"
                : "devices/p2p_participation";
        return new(isChecked, labelKey, isChecked && !device.@p2pOffRequested);
    }
}
