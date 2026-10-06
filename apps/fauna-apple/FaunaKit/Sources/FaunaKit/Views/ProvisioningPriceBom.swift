import SwiftUI

/// Shared "Bill of Materials" price-summary region for both Apple targets
/// (iOS + macOS), rendered in the top region of `nest_provisioning` above
/// the progress pipeline.
///
/// Pre-commit recap of up to two priced line items from
/// `OnboardingMachine::bill_of_materials()`: the domain's one-time
/// registration price (`provisioning-bom-domain-line`, shown only when
/// buying a new domain) and the selected VPS's recurring monthly price
/// (`provisioning-bom-vps-line`, always present once `vps_config`'s
/// Continue has been taken). Both prices were already shown/agreed earlier
/// in the wizard (`dns-tld-price-display`, the `vps_config` server-type
/// options) — this is a recap, not a new price source. Distinguishing the
/// two items by `recurring` (not vec order) mirrors the Linux
/// `views/onboarding/nest_provisioning.rs` price-bom block and the web
/// `+page.svelte` equivalent. Per `docs/goal/behavior/onboarding.md` §6.
public struct ProvisioningPriceBom: View {
    let items: [BillOfMaterialsItem]

    public init(items: [BillOfMaterialsItem]) {
        self.items = items
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            if let domain = items.first(where: { !$0.recurring }) {
                automationText(
                    Ids.provisioningBomDomainLine,
                    domainBomLine(domain))
            }
            if let vps = items.first(where: { $0.recurring }) {
                automationText(
                    Ids.provisioningBomVpsLine,
                    L.onboarding.nestProvisioning.bomLineRecurring(
                        label: renderLocalizedText(vps.label),
                        price: formatPrice(cents: vps.priceCents, currency: vps.currency)))
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.provisioningPriceBom)
    }

    /// The domain line's renewal disclosure (`onboarding.md` § 6): when the
    /// registrar quoted a `renewalPriceCents`, say the recurring cost the
    /// user is signing up for BEFORE the charge, not after — mirrors web's
    /// `+page.svelte` branch (`bomLineDomain` vs the plain `bomLine`).
    private func domainBomLine(_ domain: BillOfMaterialsItem) -> String {
        let label = renderLocalizedText(domain.label)
        let price = formatPrice(cents: domain.priceCents, currency: domain.currency)
        if let renewalPriceCents = domain.renewalPriceCents {
            return L.onboarding.nestProvisioning.bomLineDomain(
                label: label, price: price,
                renewal: formatPrice(cents: renewalPriceCents, currency: domain.currency))
        }
        return L.onboarding.nestProvisioning.bomLine(label: label, price: price)
    }
}
