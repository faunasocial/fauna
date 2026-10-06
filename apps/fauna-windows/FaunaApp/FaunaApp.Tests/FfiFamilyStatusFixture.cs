using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint an <see cref="FfiFamilyStatus"/> with named
/// overrides for just the field(s) a test cares about — the sibling of
/// <see cref="FfiFamilyWardInfoFixture"/> for the pattern.
/// <c>supervision</c> is the reply's gated supervision fold, which shared Rust
/// attaches to every real reply and which is never on the wire. A test modelling
/// what the enforcement caches should see passes it alongside
/// <c>supervisedBy</c>/<c>policy</c>, since the fold holds nothing without a
/// guardian.
/// </summary>
internal static class FfiFamilyStatusFixture
{
    internal static FfiFamilyStatus Make(
        FfiFamilyGuardianInfo? supervisedBy = null,
        FfiReachPolicy? policy = null,
        FfiFamilyWardInfo[]? wards = null,
        FfiFamilyIncomingTransfer[]? incomingTransfers = null,
        uint? usageTodayMinutes = null,
        FfiFamilyContactRequest[]? contactRequests = null,
        FfiFamilyFeedRequest[]? feedRequests = null,
        FfiFamilyAgeBand? ageBand = null,
        FfiSupervisionSnapshot? supervision = null) =>
        new FfiFamilyStatus(
            supervisedBy, policy, wards ?? [], incomingTransfers ?? [], usageTodayMinutes,
            contactRequests ?? [], feedRequests ?? [], ageBand, supervision);
}
