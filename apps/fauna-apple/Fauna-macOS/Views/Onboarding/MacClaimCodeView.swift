import SwiftUI
import FaunaKit

/// claim_code — entered only when handle-check returns
/// `UnregisteredUnclaimedNest` (the nest is reachable but
/// the `fauna.setup.status` WS-RPC kind reports `claimed=false`). The user pastes the
/// one-time claim code printed by the nest server's bootstrap process and
/// atomically becomes the admin via `POST /api/v1/claim-admin`.
///
/// Single button (Claim) — there is no Continue. On success the machine
/// advances to `nat_mode_choice` (the admin path's terminal step); the
/// container's step router renders that next. See
/// `docs/goal/behavior/onboarding.md` §3a/§3b-bis and `tests/e2e-unified/ui.yaml`
/// (`claim_code` page block).
struct MacClaimCodeView: View {
    @Bindable var vm: OnboardingVM
    @State private var code: String = ""

    var body: some View {
        let snap = vm.machine.claimCodeSnapshot()

        VStack(alignment: .leading, spacing: 12) {
            automationText(Ids.pageHeading, L.onboarding.claimCode.title)
                .font(.title2)

            Text(L.onboarding.claimCode.description)
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: 8) {
                TextField(L.onboarding.claimCode.placeholder, text: $code)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.claimCodeInput)
                    .automationField(Ids.claimCodeInput, text: $code)
                    .disableAutocorrection(true)

                Button(L.onboarding.claimCode.submitButton) {
                    Task { _ = await vm.machine.wizardSubmitClaimCode(code: code) }
                }
                // The machine's `submit_enabled` covers the in-flight /
                // result states; the view additionally gates on input
                // emptiness (a pure UI concern, per the snapshot doc).
                .keyboardShortcut(.defaultAction)
                .disabled(!snap.submitEnabled || code.isEmpty)
                .accessibilityIdentifier(Ids.claimCodeSubmitButton)
                .automationActivate(Ids.claimCodeSubmitButton,
                                    isEnabled: { vm.machine.claimCodeSnapshot().submitEnabled && !code.isEmpty }) {
                    Task { _ = await vm.machine.wizardSubmitClaimCode(code: code) }
                }
            }

            automationText(Ids.claimCodeStatus, renderLocalizedText(vm.machine.claimCodeSnapshot().message))
                .font(.callout)
                .frame(maxWidth: .infinity, minHeight: 24, alignment: .leading)

            if let error = vm.errorMessage {
                ErrorBanner(message: error)
            }

            HStack {
                Button(L.common.back) { vm.machine.back() }
                    .accessibilityIdentifier(Ids.claimCodeBackButton)
                    .automationActivate(Ids.claimCodeBackButton) { vm.machine.back() }
            }
        }
        .padding()
        .frame(minWidth: 480)
        .onAppear {
            // Pre-fill the one-time claim code the client holds after a factory
            // reset (the human never sees it — `fauna.admin.factory_reset` returns
            // it to the client, which carries it via the machine's
            // `claim_code_prefill`). Mirrors linux `claim_code.rs` (set the input
            // from `claim_code_prefill()` when present). Guard on emptiness so a
            // re-appear never clobbers what the user typed.
            if code.isEmpty, let prefill = vm.machine.claimCodePrefill() {
                code = prefill
            }
        }
    }
}
