using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Applies one <c>fauna.family.status</c> reply's supervision fold
/// (<c>FfiFamilyStatus.supervision</c>) to the three windows enforcement
/// caches — <see cref="ContentPolicyCache"/>'s guardian half, <see
/// cref="GuardianNotifyCache"/>, and <see cref="ScreenTimeCache"/> — the one
/// call <c>MainPage.CheckFamilyStatusAsync</c> makes. Lifted into
/// <c>FaunaApp.Core</c> because <c>FaunaApp.Tests</c> cannot reach
/// <c>MainPage</c>, a WinUI code-behind type.
///
/// <para>The client-enforced inputs move ONLY from <c>supervision</c>, never
/// from the raw <see cref="FfiFamilyStatus"/> policy document:
/// <c>SupervisionSnapshot::from_status</c> (family-client-enforcement.md §
/// Implementation status today) gates every field on <c>supervised_by</c>, so
/// a reply that still carries a policy but names no guardian binds nothing
/// here — the same rule android's <c>ContentPolicyStore</c> and apple's
/// <c>ContentPolicyStore</c>/<c>ScreenTimeStore</c> enforce off the same
/// fold.</para>
/// </summary>
internal static class FamilyStatusHandler
{
    internal static void ApplySupervision(FfiFamilyStatus status)
    {
        var supervision = status.@supervision;
        ContentPolicyCache.SetGuardianPolicy(supervision?.@contentPolicy);
        GuardianNotifyCache.SetEnabled(supervision?.@contentNotify ?? false);
        ScreenTimeCache.SetWardScreenTime(
            supervision?.@screenTime, supervision?.@supervisedBy.@handle, status.@usageTodayMinutes);
    }
}
