using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint an <see cref="FfiFamilyWardInfo"/> with named
/// overrides for just the field(s) a test cares about, instead of every call
/// site hand-listing all eight positionally — the sibling of
/// <see cref="FfiContentPolicyFixture"/> for the pattern. Three independent files
/// (<c>FamilyTransferRowTests</c>, <c>FamilyWardContentNoticesTests</c>,
/// <c>FamilyContentPolicyEditorTests</c>) had each grown their OWN full-field
/// construction of this type — the exact "two branches collide on the grown
/// field" risk row 48 exists to close. Widening <see cref="FfiFamilyWardInfo"/>
/// now touches exactly this one file.
/// </summary>
internal static class FfiFamilyWardInfoFixture
{
    internal static FfiFamilyWardInfo Make(
        byte[]? actorId = null,
        string handle = "ward",
        FfiReachPolicy? policy = null,
        FfiFamilyPendingTransfer? pendingTransfer = null,
        FfiFamilyContentNotice[]? contentNotices = null,
        uint? usageTodayMinutes = null,
        FfiFamilyWardDevice[]? devices = null,
        FfiFamilyAgeBand? ageBand = null,
        FfiFamilyBlockedPeer[]? blockedDmPeers = null) =>
        new FfiFamilyWardInfo(
            actorId ?? new byte[32], handle, policy ?? DefaultPolicy(), pendingTransfer,
            contentNotices ?? [], usageTodayMinutes, devices ?? [], ageBand,
            blockedDmPeers ?? []);

    private static FfiReachPolicy DefaultPolicy() =>
        new(contactApproval: true, unknownSenderMail: "allow", federationContact: true,
            feedSources: "allow", contentPolicy: null, screenTime: null,
            contentNotify: null, unknownPeerDm: null);
}
