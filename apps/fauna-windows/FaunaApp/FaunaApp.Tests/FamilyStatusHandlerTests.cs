using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;
using ContentLabelEntry = uniffi.fauna_core.ContentLabelEntry;

namespace FaunaApp.Tests;

/// <summary>
/// <see cref="FamilyStatusHandler.ApplySupervision"/> — the single call
/// <c>MainPage.CheckFamilyStatusAsync</c> makes to feed the three windows
/// enforcement caches (<see cref="ContentPolicyCache"/>, <see
/// cref="GuardianNotifyCache"/>, <see cref="ScreenTimeCache"/>) from one
/// <c>fauna.family.status</c> reply, lifted here because
/// <c>FaunaApp.Tests</c> cannot reach the WinUI <c>MainPage</c> code-behind.
///
/// <para>Pins family-client-enforcement.md's rule directly: the client-
/// enforced inputs move ONLY from <c>FfiFamilyStatus.supervision</c> (the
/// shared <c>SupervisionSnapshot::from_status</c> fold, gated on
/// <c>supervised_by</c>), never from the raw <c>status.policy</c> — a reply
/// that still carries a policy document but names no guardian must bind
/// nothing enforceable.</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class FamilyStatusHandlerTests : IDisposable
{
    public FamilyStatusHandlerTests()
    {
        ContentPolicyCache.Reset();
        GuardianNotifyCache.Reset();
        ScreenTimeCache.Reset();
    }

    public void Dispose()
    {
        ContentPolicyCache.Reset();
        GuardianNotifyCache.Reset();
        ScreenTimeCache.Reset();
    }

    private static ContentLabelEntry[] Labels(params (string Category, ushort Permille)[] entries)
    {
        var result = new ContentLabelEntry[entries.Length];
        for (var i = 0; i < entries.Length; i++)
        {
            result[i] = new ContentLabelEntry(entries[i].Category, entries[i].Permille);
        }
        return result;
    }

    /// <summary>
    /// The defect this row fixes: a successful status whose raw <c>policy</c>
    /// names a floor, Notify and a lock-worthy window, but with no guardian
    /// and so no fold, must bind NONE of the three — proving the handler
    /// reads <c>supervision</c>, not <c>policy</c>.
    /// </summary>
    [Fact]
    public void ApplySupervision_PolicyPresentButNoFold_BindsNothingEnforceable()
    {
        var status = FfiFamilyStatusFixture.Make(
            supervisedBy: null,
            policy: new FfiReachPolicy(
                contactApproval: false,
                unknownSenderMail: "allow",
                federationContact: false,
                feedSources: "allow",
                contentPolicy: FfiContentPolicyFixture.Make(nsfw: "block"),
                screenTime: FfiScreenTimePolicyFixture.Make(dailyMinutes: 0),
                contentNotify: true,
                unknownPeerDm: null),
            usageTodayMinutes: 5,
            supervision: null);

        FamilyStatusHandler.ApplySupervision(status);

        Assert.Equal("show", ContentPolicyCache.VerdictFor(Labels(("nsfw", 900))));
        GuardianNotifyCache.Record("post-1", ["nsfw"]);
        Assert.Null(GuardianNotifyCache.TryTakeDue());
        Assert.Null(ScreenTimeCache.LockMessage);
    }

    /// <summary>
    /// The green side of the same rule: with a real fold present, all three
    /// caches bind from it, keyed on its own guardian/content/screen-time
    /// fields rather than the raw policy.
    /// </summary>
    [Fact]
    public void ApplySupervision_FoldPresent_BindsFloorNotifyAndLockFromTheFold()
    {
        var guardian = new FfiFamilyGuardianInfo(actorId: new byte[] { 1 }, handle: "guardian-1");
        var supervision = new FfiSupervisionSnapshot(
            supervisedBy: guardian,
            contentPolicy: FfiContentPolicyFixture.Make(nsfw: "block"),
            contentNotify: true,
            screenTime: FfiScreenTimePolicyFixture.Make(dailyMinutes: 0));
        var status = FfiFamilyStatusFixture.Make(
            supervisedBy: guardian,
            usageTodayMinutes: 5,
            supervision: supervision);

        FamilyStatusHandler.ApplySupervision(status);

        Assert.Equal("block", ContentPolicyCache.VerdictFor(Labels(("nsfw", 900))));
        GuardianNotifyCache.Record("post-1", ["nsfw"]);
        Assert.NotNull(GuardianNotifyCache.TryTakeDue());
        Assert.NotNull(ScreenTimeCache.LockMessage);
    }
}
