namespace FaunaApp.Core.ViewModels;

/// <summary>
/// One row of the <c>nest_provisioning</c> page's DNS plan, built by
/// <see cref="OnboardingViewModel.ProvisioningDnsPlanRows"/> from the shared
/// <c>OnboardingMachine::dns_plan()</c> snapshot
/// (<c>onboarding-provisioning.md</c> § 6 <i>The DNS plan</i>).
/// <see cref="Key"/> is the record's <c>&lt;name&gt;|&lt;type&gt;</c>, which
/// the view puts in <c>provisioning-dns-plan-row[&lt;key&gt;]</c>; <see cref="Text"/>
/// is the row's sentence, already resolved. Public, because a WinUI
/// <c>x:Bind</c> to an internal type binds nothing.
/// </summary>
public sealed record ProvisioningDnsPlanRowItem(string Key, string Text);
