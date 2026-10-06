using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint an <see cref="FfiScreenTimePolicy"/> with named overrides
/// for just the field(s) a test cares about, instead of every call site
/// hand-listing all three positionally. Widening <see cref="FfiScreenTimePolicy"/> now touches exactly this one file.
/// </summary>
internal static class FfiScreenTimePolicyFixture
{
    internal static FfiScreenTimePolicy Make(
        ushort? windowStart = null,
        ushort? windowEnd = null,
        ushort? dailyMinutes = null) =>
        new FfiScreenTimePolicy(windowStart, windowEnd, dailyMinutes);
}
