import SwiftUI

/// `kids_ineligible` — the Fauna Kids build's refusal for an account that is not
/// supervised (`account-age-band.md` § The account age band, the kids-app
/// bullet, item (3)). Replaces the whole signed-in shell when the shared
/// `kidsAppEligible` verdict (`FamilyStatusStore.kidsEligible`) is false; the
/// verdict keys on supervision, never on the band. One explanation and a
/// sign-out — nothing else is reachable. iOS twin of android's
/// `KidsIneligibleScreen`.
///
/// The sign-out is the ordinary one: the shared ``SignOutSection`` (inline
/// confirm, the `signOutBlocked` probe, and its own `error-message` line on a
/// refusal), so the credential wipe and re-root run the same path Settings →
/// Account does.
///
/// Mounted only behind `#if FAUNA_KIDS` by the iOS app root; the view itself is
/// build-flavor-neutral so the FaunaKit tests can drive it.
public struct KidsIneligibleView: View {
    private let session: SessionState
    private let onSignedOut: () -> Void

    public init(session: SessionState, onSignedOut: @escaping () -> Void) {
        self.session = session
        self.onSignedOut = onSignedOut
    }

    public var body: some View {
        VStack(spacing: 24) {
            Text(L.family.kidsIneligibleMessage)
                .font(.body)
                .multilineTextAlignment(.center)
                .accessibilityIdentifier(Ids.kidsIneligibleMessage)
                .automationValue(Ids.kidsIneligibleMessage, text: { L.family.kidsIneligibleMessage })
            // This gate replaces the whole shell, so it holds no composer to flush.
            SignOutSection(session: session, flushDraftRails: {}, onSignedOut: onSignedOut)
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
