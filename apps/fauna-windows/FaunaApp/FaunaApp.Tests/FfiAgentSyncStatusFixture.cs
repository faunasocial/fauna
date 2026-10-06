using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint an <see cref="FfiAgentSyncStatus"/> with named overrides
/// for just the field(s) a test cares about, instead of the call site
/// hand-listing all five positionally. Widening <see cref="FfiAgentSyncStatus"/> now touches exactly this one file.
/// </summary>
internal static class FfiAgentSyncStatusFixture
{
    internal static FfiAgentSyncStatus Make(
        bool connected = true,
        bool syncing = false,
        ulong filesPending = 0,
        ulong bytesPending = 0,
        ulong? lastSync = null) =>
        new FfiAgentSyncStatus(connected, syncing, filesPending, bytesPending, lastSync);
}
