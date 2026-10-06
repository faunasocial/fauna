import SwiftUI

/// The onboarding **DNS-config** step's shared field sequence
/// (`docs/goal/behavior/onboarding.md` § dns_config): buy-domain checkbox,
/// the no-provider-carries-TLD warning, same-provider checkbox, `ProviderRow`,
/// "set up later" + its warning, and the provider-specific expansion
/// (`dnsProviderSection`). macOS (`MacDnsConfigView`) and iOS (`DnsConfigView`)
/// rendered this near-identically except for the no-provider-message's visual
/// position — a real, user-ratified 2026-07-18 divergence (macOS/android showed
/// it up-front, iOS/web showed it low; converged on the up-front placement,
/// `onboarding.md` § dns_config). Lifted to one shared FaunaKit definition now
/// that both platforms agree on the order.
///
/// **Why a free `@ViewBuilder` function and not a `struct … : View`?** Same
/// reason as `dnsProviderSection`: the body reads machine snapshots via
/// `vm.machine.<getter>()` — untracked `let` accesses `@Observable` does NOT
/// subscribe to. Inlined into the caller's `body`, it re-evaluates on every
/// machine change for free. A child `View` struct holding `vm` would read
/// nothing tracked and go stale after `toggleBuyDomain()` etc.
///
/// **No wrapping container** — the caller's own `VStack(alignment: .leading,
/// spacing: …)` supplies the sequence's spacing (iOS and macOS use slightly
/// different values), so this stays a bare sibling sequence, not a nested
/// `VStack` with its own hardcoded spacing.
@ViewBuilder
public func dnsConfigFields(vm: OnboardingVM) -> some View {
    Toggle(L.onboarding.dnsConfig.buyDomainCheckbox, isOn: Binding(
        get: { vm.machine.dnsConfig().buyDomain },
        set: { vm.machine.toggleBuyDomain(on: $0) }
    ))
        .accessibilityIdentifier(Ids.dnsBuyDomainCheckbox)
        // A toggle's "click" is a flip — invert the live checked state through
        // the same machine setter (mirrors MacHandleEntryView's
        // handle-control-checkbox; a bare Toggle is invisible in-process).
        .automationActivate(Ids.dnsBuyDomainCheckbox) {
            vm.machine.toggleBuyDomain(on: !vm.machine.dnsConfig().buyDomain)
        }

    if vm.machine.shouldShowNoProviderMessage() {
        automationText(Ids.dnsNoProviderMessage, L.onboarding.dnsConfig.noProviderCarriesTld(tld: handleTld(handle: vm.machine.currentHandle()) ?? ""))
            .font(.subheadline)
            .foregroundStyle(.secondary)
    }

    Toggle(L.onboarding.dnsConfig.sameProviderCheckbox, isOn: Binding(
        get: { vm.machine.dnsConfig().sameProviderForVps },
        set: { vm.machine.toggleSameProviderForVps(on: $0) }
    ))
        .accessibilityIdentifier(Ids.dnsSameProviderCheckbox)
        .automationActivate(Ids.dnsSameProviderCheckbox) {
            vm.machine.toggleSameProviderForVps(on: !vm.machine.dnsConfig().sameProviderForVps)
        }

    ProviderRow(
        providers: PROVIDERS.filter { $0.capabilities.contains(.dns) },
        selectedId: vm.machine.dnsConfig().selectedProviderId,
        kind: "dns",
        isEnabled: { vm.machine.dnsProviderEligible(providerId: $0.id) },
        reasonFor: { vm.machine.dnsProviderIneligibleReason(providerId: $0.id) },
        onSelect: { vm.machine.selectDnsProvider(id: $0) }
    )

    Button(L.onboarding.dnsConfig.setUpLater) { vm.machine.dnsSetUpLater() }
        .accessibilityIdentifier(Ids.dnsSetUpLaterButton)
        .automationActivate(Ids.dnsSetUpLaterButton) { vm.machine.dnsSetUpLater() }
    Text(L.onboarding.dnsConfig.setUpLaterWarning)
        .font(.caption)
        .foregroundStyle(.secondary)

    if let providerId = vm.machine.dnsConfig().selectedProviderId,
       let provider = PROVIDERS.first(where: { $0.id == providerId }) {
        dnsProviderSection(vm: vm, provider: provider)
    }
}
