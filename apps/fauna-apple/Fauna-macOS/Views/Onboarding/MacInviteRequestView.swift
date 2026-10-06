import SwiftUI
import FaunaKit

/// invite_request — single-page invite flow per `tests/e2e-unified/ui.yaml`
/// and a design tracked internally.
///
/// Two independent affordances:
///   - Top row: Request invite + status + (optional) Recheck for PendingReview.
///   - Bottom row: out-of-band invite code input + Check + status.
/// Continue is enabled when state is Approved | PendingReview, or when the
/// out-of-band code state is Valid. The wizard exit path is chosen by the
/// machine (`redeem_invite` for Approved/OOB-valid → LoggedIn;
/// PendingReview has no Continue at all — that journey polls).
struct MacInviteRequestView: View {
    @Bindable var vm: OnboardingVM
    @State private var oobCode: String = ""

    var body: some View {
        let snap = vm.machine.inviteRequestSnapshot()

        VStack(alignment: .leading, spacing: 16) {
            automationText(Ids.pageHeading, L.onboarding.inviteRequest.title)
                .font(.title2)

            // ── Top row: request invite ──────────────────────────────
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Button(L.onboarding.invite.requestButton) { vm.submitInviteRequest() }
                    .accessibilityIdentifier(Ids.inviteRequestSubmitButton)
                    .automationActivate(Ids.inviteRequestSubmitButton) { vm.submitInviteRequest() }

                    if snap.recheckVisible {
                        Button(L.onboarding.invite.recheckButton) { vm.recheckInvite() }
                        .accessibilityIdentifier(Ids.inviteRequestRecheckButton)
                        .automationActivate(Ids.inviteRequestRecheckButton) { vm.recheckInvite() }
                    }
                }

                Text(renderLocalizedText(snap.message))
                    .font(.callout)
                    .frame(maxWidth: .infinity, minHeight: 24, alignment: .leading)
                    .accessibilityIdentifier(Ids.inviteRequestStatus)
                    .automationValue(Ids.inviteRequestStatus,
                                     text: { renderLocalizedText(vm.machine.inviteRequestSnapshot().message) })
            }

            Divider()

            // ── Bottom row: out-of-band invite code ──────────────────
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    TextField(L.onboarding.oobCode.placeholder, text: $oobCode)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityIdentifier(Ids.inviteCodeInput)
                        .automationField(Ids.inviteCodeInput, text: $oobCode)
                        .disableAutocorrection(true)
                    Button(L.common.check) { vm.checkOobCode(oobCode) }
                    .disabled(oobCode.isEmpty)
                    .accessibilityIdentifier(Ids.inviteCodeCheckButton)
                    .automationActivate(Ids.inviteCodeCheckButton,
                                        isEnabled: { !oobCode.isEmpty }) { vm.checkOobCode(oobCode) }
                }

                Text(renderLocalizedText(snap.oobMessage))
                    .font(.callout)
                    .frame(maxWidth: .infinity, minHeight: 24, alignment: .leading)
                    .accessibilityIdentifier(Ids.inviteCodeStatus)
                    .automationValue(Ids.inviteCodeStatus,
                                     text: { renderLocalizedText(vm.machine.inviteRequestSnapshot().oobMessage) })

                guardianSupervisedNotice(for: snap.outOfBandCodeState, font: .callout)
            }

            if let error = vm.errorMessage {
                ErrorBanner(message: error)
            }

            HStack {
                Button(L.common.back) { vm.inviteBack() }
                .accessibilityIdentifier(Ids.inviteRequestBackButton)
                .automationActivate(Ids.inviteRequestBackButton) { vm.inviteBack() }

                Button(L.common.continue) { vm.submitInviteContinue() }
                .keyboardShortcut(.defaultAction)
                .disabled(!vm.inviteContinueEnabled())
                .accessibilityIdentifier(Ids.inviteRequestContinueButton)
                .automationActivate(Ids.inviteRequestContinueButton,
                                    isEnabled: { vm.inviteContinueEnabled() }) {
                    vm.submitInviteContinue()
                }
            }
        }
        .padding()
        .frame(minWidth: 480)
        .task(id: vm.isInvitePendingReview) {
            guard vm.isInvitePendingReview else { return }
            await vm.pollPendingInviteWhileNeeded()
        }
    }
}
