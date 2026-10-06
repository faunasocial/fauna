import SwiftUI
import FaunaKit

/// invite_request — single-page invite flow per
/// `docs/goal/behavior/onboarding.md` §3 (design tracked internally).
///
/// Two independent affordances:
///   - Top row: Request invite + status + (optional) Recheck for PendingReview.
///   - Bottom row: out-of-band invite code input + Check + status.
/// Continue is enabled when state is Approved | PendingReview, or when the
/// out-of-band code state is Valid. The wizard exit path is chosen by the
/// machine (`redeem_invite` for Approved/OOB-valid → LoggedIn;
/// PendingReview has no Continue at all — that journey polls).
struct InviteRequestView: View {
    @Bindable var vm: OnboardingVM
    @State private var oobCode: String = ""

    var body: some View {
        let snap = vm.machine.inviteRequestSnapshot()

        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                automationText(Ids.pageHeading, L.onboarding.inviteRequest.title)
                    .font(.title2.bold())

                Text(L.onboarding.inviteRequest.subtitle)
                    .foregroundStyle(.secondary)

                // The store-age round's outcome, before submit/redeem — derived in
                // the shared machine from the claim the platform glue hands
                // `set_age_claim`; `nil` = the store shared nothing, so nothing
                // paints (family-safety.md § App surface → *Age-band surfaces*;
                // a `platform_elements` entry for android/ios — macOS declares
                // the absence).
                if let notice = snap.ageNotice {
                    automationText(Ids.inviteRequestAgeNotice, renderLocalizedTextNested(notice))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }

                requestInviteSection(snap: snap)
                Divider()
                outOfBandCodeSection(snap: snap)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                Button(L.common.continue) { vm.submitInviteContinue() }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .frame(maxWidth: .infinity)
                .disabled(!vm.inviteContinueEnabled())
                .accessibilityIdentifier(Ids.inviteRequestContinueButton)
                .automationActivate(Ids.inviteRequestContinueButton,
                                    isEnabled: { vm.inviteContinueEnabled() }) {
                    vm.submitInviteContinue()
                }
            }
            .padding()
        }
        // The store-age round (Declared Age Range → band → App Attest) runs as
        // the page appears, so its notice above paints before submit/redeem.
        .task { await vm.attachStoreAgeClaim(signals: AppleStoreAgeSignals()) }
        .task(id: vm.isInvitePendingReview) {
            guard vm.isInvitePendingReview else { return }
            await vm.pollPendingInviteWhileNeeded()
        }
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button(L.common.back) { vm.inviteBack() }
                .accessibilityIdentifier(Ids.inviteRequestBackButton)
                .automationActivate(Ids.inviteRequestBackButton) { vm.inviteBack() }
            }
        }
    }

    @ViewBuilder
    private func requestInviteSection(snap: InviteRequestSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Button(L.onboarding.invite.requestButton) { vm.submitInviteRequest() }
                .buttonStyle(.bordered)
                .accessibilityIdentifier(Ids.inviteRequestSubmitButton)
                .automationActivate(Ids.inviteRequestSubmitButton) { vm.submitInviteRequest() }

                if snap.recheckVisible {
                    Button(L.onboarding.invite.recheckButton) { vm.recheckInvite() }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier(Ids.inviteRequestRecheckButton)
                    .automationActivate(Ids.inviteRequestRecheckButton) { vm.recheckInvite() }
                }
            }

            Text(renderLocalizedText(snap.message))
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityIdentifier(Ids.inviteRequestStatus)
                .automationValue(Ids.inviteRequestStatus,
                                 text: { renderLocalizedText(vm.machine.inviteRequestSnapshot().message) })
        }
    }

    @ViewBuilder
    private func outOfBandCodeSection(snap: InviteRequestSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                TextField(L.onboarding.oobCode.placeholder, text: $oobCode)
                    .textFieldStyle(.roundedBorder)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .accessibilityIdentifier(Ids.inviteCodeInput)
                    .automationField(Ids.inviteCodeInput, text: $oobCode)

                Button(L.common.check) { vm.checkOobCode(oobCode) }
                .buttonStyle(.bordered)
                .disabled(oobCode.isEmpty)
                .accessibilityIdentifier(Ids.inviteCodeCheckButton)
                .automationActivate(Ids.inviteCodeCheckButton,
                                    isEnabled: { !oobCode.isEmpty }) { vm.checkOobCode(oobCode) }
            }

            Text(renderLocalizedText(snap.oobMessage))
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityIdentifier(Ids.inviteCodeStatus)
                .automationValue(Ids.inviteCodeStatus,
                                 text: { renderLocalizedText(vm.machine.inviteRequestSnapshot().oobMessage) })

            guardianSupervisedNotice(for: snap.outOfBandCodeState, font: .subheadline)
        }
    }
}
