import SwiftUI

#if !FAUNA_EXCISE_PAYMENTS
/// The claim-redeem pair — `subscription-claim-redeem-input` +
/// `subscription-claim-redeem-button` (`monetization.md` § Pillar 3 → *Which
/// nest an in-app redeem binds on* → *The doors*). One row, two doors: the
/// `subscription-settings` page (no creator in hand, the local redeem) and the
/// OTHER profile Tiers tab (this creator's code, redeemed at the page's actor
/// home). The view model behind each door owns the buffer and the call; this
/// row is the shared renderer, so both doors read the same strings, ids and
/// Dim-3 gate.
struct ClaimRedeemRow: View {
    @Binding var code: String
    let onRedeem: () -> Void

    var body: some View {
        HStack {
            TextField(L.subscriptions.claimCode, text: $code)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.subscriptionClaimRedeemInput)
                .automationField(Ids.subscriptionClaimRedeemInput, text: $code)
            Button(L.subscriptions.redeem) { onRedeem() }
                .accessibilityIdentifier(Ids.subscriptionClaimRedeemButton)
                .automationActivate(Ids.subscriptionClaimRedeemButton) { onRedeem() }
                // The code field beside it is buffer and stays typeable.
                .faunaGate("fauna.payments.claims.redeem")
        }
    }
}
#endif
