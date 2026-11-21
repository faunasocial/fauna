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
///
/// `answer` is the door's features gate over the `payments` row
/// (``featureGateAnswer(rows:failed:feature:)``, `profile.md` § Errors & edge
/// cases): blocked still loading until the read answers, blocked with a
/// `disabled` row's restriction, and absent — the row renders nothing — on a
/// nest with no `payments` plane.
struct ClaimRedeemRow: View {
    @Binding var code: String
    let answer: Possibility
    let onRedeem: () -> Void

    var body: some View {
        if answer.isShown {
            row
            FaunaPossibilityReason(answer)
        }
    }

    private var row: some View {
        HStack {
            TextField(L.subscriptions.claimCode, text: $code)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.subscriptionClaimRedeemInput)
                .automationField(Ids.subscriptionClaimRedeemInput, text: $code)
            Button(L.subscriptions.redeem) { onRedeem() }
                .accessibilityIdentifier(Ids.subscriptionClaimRedeemButton)
                .automationActivate(Ids.subscriptionClaimRedeemButton) { onRedeem() }
                // The draft half (`fauna_client_subscriptions::forms::
                // redeem_code_possibility`, `profile.md` § Errors & edge
                // cases) folded with the payments gate below; each door's
                // verb asks it again and refuses on its `error-message`.
                .faunaPossible(subscriptionRedeemCodePossibility(code: code))
                // The code field beside it is buffer and stays typeable.
                .faunaGate("fauna.payments.claims.redeem")
                .faunaPossible(answer)
        }
    }
}
#endif
