import SwiftUI

/// The `dns_config` buy-domain price-confirm control — `type: button` in
/// ui.yaml, not a real checkbox: linux's reference `CheckButton` never syncs
/// its checked state back either (`confirm_price` is fire-and-forget, and
/// `canContinueDns()` alone gates Continue). One generic withdrawal-
/// acknowledgement string for every registrar (`docs/goal/behavior/onboarding.md`
/// § 4). A pure leaf view, mirroring `DnsContactForm`/`DnsRegistrarNotes` —
/// the *gating* (`buyDomain && buyable`) stays in the caller.
public struct DnsPriceConfirmButton: View {
    public let onConfirm: () -> Void

    public init(onConfirm: @escaping () -> Void) {
        self.onConfirm = onConfirm
    }

    public var body: some View {
        Button(L.registrar.priceConfirm) { onConfirm() }
            .accessibilityIdentifier(Ids.dnsPriceConfirmCheckbox)
            .automationActivate(Ids.dnsPriceConfirmCheckbox, text: { L.registrar.priceConfirm }) {
                onConfirm()
            }
    }
}
