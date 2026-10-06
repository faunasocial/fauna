using System;

namespace FaunaApp.Core.Services;

/// <summary>
/// The device's UTC offset — the ONE place windows reads it
/// (<c>value-formatting.md</c> § Absolute local timestamp display, the
/// app-side one-door rule). Shared Rust is WASM-safe and owns no clock, so
/// every app supplies the offset itself — and supplies it from one door,
/// because the value is not display-only: it rides
/// <c>fauna.family.usage_report</c> / <c>notify_report</c>, where the nest
/// persists it as the ward-local day bucket, so a second in-app derivation is
/// drift waiting to disagree with the first. <see cref="TimeZoneInfo"/> is
/// the ratified windows sourcing API (<c>value-formatting.md:162</c>); do not
/// reintroduce a second <c>DateTimeOffset.Now.Offset</c> /
/// <c>TimeZoneInfo.Local.GetUtcOffset(...)</c> read at any call site. Mirrors
/// apple <c>DeviceOffset.swift</c> / android <c>DeviceOffset.kt</c>.
/// </summary>
internal static class DeviceOffset
{
    /// <summary>Seconds east of UTC — the unit the shared formatters take.</summary>
    public static int UtcOffsetSeconds()
        => (int)TimeZoneInfo.Local.GetUtcOffset(DateTime.Now).TotalSeconds;

    /// <summary>Minutes east of UTC — the unit the family day-bucket rule
    /// wants (<c>family-safety.md</c> § Screen time / Guardian Notify).</summary>
    public static int UtcOffsetMinutes() => UtcOffsetSeconds() / 60;
}
