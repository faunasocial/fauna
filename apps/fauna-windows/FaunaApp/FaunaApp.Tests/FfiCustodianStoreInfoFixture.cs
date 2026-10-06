using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint an <see cref="FfiCustodianStoreInfo"/> with named overrides
/// for just the field(s) a test cares about, instead of the call site
/// hand-listing all three positionally. Widening <see cref="FfiCustodianStoreInfo"/> now touches exactly this one file.
/// </summary>
internal static class FfiCustodianStoreInfoFixture
{
    internal static FfiCustodianStoreInfo Make(
        ulong generations = 1,
        ulong files = 3,
        ulong bytes = 0) =>
        new FfiCustodianStoreInfo(
            generations, files, bytes, Array.Empty<FfiCustodianSourceRegression>());
}
