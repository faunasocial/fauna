import SwiftUI

/// The provider-specific expansion of the onboarding **DNS-config** step
/// (`docs/goal/behavior/onboarding.md` § dns_config): signup link +
/// open-in-browser, provider help text, the generated credentials sub-form, a
/// verify button, the live status text, the TLD price, and the
/// conditionally-shown registrar notes + WHOIS contact form. macOS
/// (`MacDnsConfigView`) and iOS (`DnsConfigView`) rendered this near-identically;
/// lifted to one shared FaunaKit definition. Every
/// visibility rule (`visible_dns_fields` / `should_show_registrar_notes` /
/// `should_show_contact_form`) comes from the shared `OnboardingMachine` and is
/// never re-derived in the per-app shell. Element IDs match
/// `tests/e2e-unified/ui.yaml` `dns_config` exactly.
///
/// **Why a free `@ViewBuilder` function and not a `struct … : View`?** The body
/// reads machine snapshots via `vm.machine.<getter>()` — untracked `let`
/// accesses that SwiftUI's `@Observable` tracking does NOT subscribe to.
/// Inlined into the caller's `body` (which subscribes to the VM's observation
/// tick by reading a convenience getter such as `vm.errorMessage`), it
/// re-evaluates on every machine change for free — exactly as the per-app
/// `providerSection` methods did before the lift. A child `View` struct holding
/// `vm` would read nothing tracked and so go stale after `verifyDns()` etc. (the
/// same untracked-read stale-render class the RecipientPicker / backup-
/// destination fixes hit).
@ViewBuilder
public func dnsProviderSection(vm: OnboardingVM, provider: ProviderMeta) -> some View {
    VStack(alignment: .leading, spacing: 8) {
        if let url = URL(string: provider.signupUrl) {
            Link(provider.signupUrl, destination: url)
                .accessibilityIdentifier(Ids.dnsProviderLink)
                .automationValue(Ids.dnsProviderLink, text: { provider.signupUrl })
            Button(L.onboarding.dnsConfig.openInBrowser) { OpenURL.open(url) }
                .accessibilityIdentifier(Ids.dnsProviderOpenBrowserButton)
                .automationActivate(Ids.dnsProviderOpenBrowserButton) { OpenURL.open(url) }
        }
        automationText(Ids.dnsProviderHelpText, L.lookup(provider.helpKey))

        CredentialsForm(
            fields: vm.machine.visibleDnsFields(),
            kind: "dns",
            getCred: { vm.machine.dnsConfig().creds[$0] ?? "" },
            setCred: { vm.machine.setDnsCred(fieldId: $0, value: $1) },
            machine: vm.machine,
            form: .dns
        )

        Button(L.provisioning.verifyCredentials) {
            Task { try? await vm.machine.verifyDns() }
        }
        .buttonStyle(.bordered)
        .disabled(!vm.machine.canVerifyDns())
        .accessibilityIdentifier(Ids.dnsVerifyButton)
        .automationActivate(Ids.dnsVerifyButton,
                            isEnabled: { vm.machine.canVerifyDns() }) {
            Task { try? await vm.machine.verifyDns() }
        }

        automationText(Ids.dnsStatusText, renderLocalizedText(vm.machine.dnsStatusTextKey()))

        if vm.machine.dnsConfig().buyDomain,
           case .buyable(let priceCents, let currency, _) = vm.machine.dnsConfig().currentAvailability {
            automationText(Ids.dnsTldPriceDisplay, formatPrice(cents: priceCents, currency: currency ?? "USD"))

            DnsPriceConfirmButton(onConfirm: { vm.machine.confirmPrice() })
        }

        // Per onboarding.md §4: provider notes (e.g. Porkbun) and the WHOIS
        // contact form (e.g. Gandi) render only when the shared predicates say
        // so. The notes TEXT stays platform-side, read from the generated
        // provider registry's i18n key.
        if vm.machine.shouldShowRegistrarNotes(), let key = provider.registrarNotesKey {
            DnsRegistrarNotes(notesKey: key)
        }
        if vm.machine.shouldShowContactForm() {
            DnsContactForm(
                contact: vm.machine.dnsConfig().contact,
                onSet: { vm.machine.setContact(contact: $0) }
            )
        }
    }
}
