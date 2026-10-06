using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint an <see cref="FfiFamilyUsageReport"/> with named overrides
/// for just the field(s) a test cares about, instead of every call site
/// hand-listing both positionally. Widening <see cref="FfiFamilyUsageReport"/> now touches exactly this one file.
/// </summary>
internal static class FfiFamilyUsageReportFixture
{
    internal static FfiFamilyUsageReport Make(
        long day = 0,
        uint dayTotalMinutes = 0) =>
        new FfiFamilyUsageReport(day, dayTotalMinutes);
}
