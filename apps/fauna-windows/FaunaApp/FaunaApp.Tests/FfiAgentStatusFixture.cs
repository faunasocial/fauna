using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint an <see cref="FfiAgentStatus"/> with named overrides
/// for just the field(s) a test cares about, instead of every call site
/// hand-listing all three positionally. Widening <see cref="FfiAgentStatus"/> now touches exactly this one file.
/// </summary>
internal static class FfiAgentStatusFixture
{
    internal static FfiAgentStatus Make(
        FfiAgentHealthState state = FfiAgentHealthState.Running,
        string version = "0.0.0-test",
        ulong uptimeSecs = 0) =>
        new FfiAgentStatus(state, version, uptimeSecs);
}
