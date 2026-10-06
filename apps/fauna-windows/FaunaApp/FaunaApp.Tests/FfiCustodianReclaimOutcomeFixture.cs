using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint an <see cref="FfiCustodianReclaimOutcome"/> with named
/// overrides for just the field(s) a test cares about, instead of the call site
/// hand-listing all three positionally. Widening <see cref="FfiCustodianReclaimOutcome"/> now touches exactly this
/// one file.
/// </summary>
internal static class FfiCustodianReclaimOutcomeFixture
{
    internal static FfiCustodianReclaimOutcome Make(
        bool stillHosting = false,
        ulong freedFiles = 3,
        ulong freedBytes = 0) =>
        new FfiCustodianReclaimOutcome(stillHosting, freedFiles, freedBytes);
}
