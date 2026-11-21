#if !FAUNA_KIDS
import SwiftUI
import FaunaKit

/// claim_code — entered only when handle-check returns
/// `UnregisteredUnclaimedNest` (the nest is reachable but
/// the `fauna.setup.status` WS-RPC kind reports `claimed=false`). The user pastes the
/// one-time claim code printed by the nest server's bootstrap process and
/// atomically becomes the admin via `POST /api/v1/claim-admin`.
///
/// Single button (Claim) — no Continue. On success the machine advances to
/// `nat_mode_choice` and the router renders that next. See
/// `docs/goal/behavior/onboarding.md` §3a/§3b-bis and `tests/e2e-unified/ui.yaml`.
struct ClaimCodeView: View {
    @Bindable var vm: OnboardingVM
    @State private var code: String = ""

    var body: some View {
        let snap = vm.machine.claimCodeSnapshot()

        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.pageHeading, L.onboarding.claimCode.title)
                    .font(.title2.bold())

                Text(L.onboarding.claimCode.description)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)

                HStack(spacing: 8) {
                    TextField(L.onboarding.claimCode.placeholder, text: $code)
                        .textFieldStyle(.roundedBorder)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .submitLabel(.go)
                        .accessibilityIdentifier(Ids.claimCodeInput)
                        .automationField(Ids.claimCodeInput, text: $code)
                        .onSubmit { submit() }

                    Button(L.onboarding.claimCode.submitButton) { submit() }
                        .buttonStyle(.bordered)
                        .accessibilityIdentifier(Ids.claimCodeSubmitButton)
                        .automationActivate(Ids.claimCodeSubmitButton) { submit() }
                        // One shared answer — a code typed, no claim on its
                        // way; `submitClaimCode` re-asks it.
                        .faunaPossible(vm.claimCodePossibility(code))
                }
                FaunaPossibilityReason(vm.claimCodePossibility(code))

                Text(renderLocalizedText(snap.message))
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityIdentifier(Ids.claimCodeStatus)
                    .automationValue(Ids.claimCodeStatus,
                                     text: { renderLocalizedText(vm.machine.claimCodeSnapshot().message) })

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                TermsNotice()
            }
            .padding()
        }
        .onAppear {
            // Pre-fill the one-time claim code the client holds after a factory
            // reset (the human never sees it — carried via the machine's
            // `claim_code_prefill`). Mirrors linux `claim_code.rs` + macOS
            // `MacClaimCodeView`. Guard on emptiness so a re-appear never clobbers
            // what the user typed.
            if code.isEmpty, let prefill = vm.machine.claimCodePrefill() {
                code = prefill
            }
        }
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button(L.common.back) { vm.machine.back() }
                    .accessibilityIdentifier(Ids.claimCodeBackButton)
                    .automationActivate(Ids.claimCodeBackButton) { vm.machine.back() }
            }
        }
    }

    private func submit() {
        vm.submitClaimCode(code)
    }
}
#endif // !FAUNA_KIDS
