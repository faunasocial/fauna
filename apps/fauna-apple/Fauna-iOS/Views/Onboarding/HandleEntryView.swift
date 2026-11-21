import SwiftUI
import FaunaKit

struct HandleEntryView: View {
    @Bindable var vm: OnboardingVM

    var body: some View {
        let snap = vm.machine.handleCheckSnapshot()
        // One binding, used by both the TextField and `automationField` (no
        // duplication) — writes route through the same machine setter a keystroke
        // would (apple-e2e-automation.md § Resolved design point).
        let handleBinding = Binding(
            get: { vm.machine.currentHandle() },
            set: { vm.machine.setCurrentHandle(h: $0) }
        )

        VStack(alignment: .leading, spacing: 16) {
            automationText(Ids.pageHeading, L.onboarding.handle.prompt)
                .font(.title2.bold())

            Text(L.onboarding.handle.examplesHelp)
                .font(.subheadline)
                .textSelection(.enabled)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity, alignment: .center)

            Text(L.onboarding.handle.localhostHint)
                .font(.caption)
                .textSelection(.enabled)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity, alignment: .center)

            HStack(spacing: 8) {
                TextField("alice@example.com", text: handleBinding)
                .textFieldStyle(.roundedBorder)
                .keyboardType(.emailAddress)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.go)
                .accessibilityIdentifier(Ids.handleInput)
                .automationField(Ids.handleInput, text: handleBinding)
                .onSubmit { vm.runHandleCheck() }

                Button(L.common.check) { vm.runHandleCheck() }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier(Ids.handleCheckButton)
                    .automationActivate(Ids.handleCheckButton) { vm.runHandleCheck() }
                    // A handle typed, no check in flight; `runHandleCheck` re-asks it.
                    .faunaPossible(vm.handleCheckAnswer)
            }
            FaunaPossibilityReason(vm.handleCheckAnswer)

            Text(renderLocalizedText(snap.message))
                .font(.subheadline)
                // Read-only text is selectable (ui/README.md § Text the person reads
                // can be copied).
                .textSelection(.enabled)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityIdentifier(Ids.handleMessageArea)
                .automationValue(Ids.handleMessageArea,
                                 text: { renderLocalizedText(vm.machine.handleCheckSnapshot().message) })

            if let error = vm.errorMessage {
                ErrorBanner(message: error)
            }

            // The by-continuing notice only while Continue signs in — the
            // shared `terms_notice_visible` (true iff `AlreadyOnNest`).
            if snap.termsNoticeVisible {
                JoiningNestRules(summary: vm.joiningNestRules)
                TermsNotice()
            }

            Button(L.common.continue) { submitContinue() }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .frame(maxWidth: .infinity)
            .accessibilityIdentifier(Ids.handleEntryContinueButton)
            .automationActivate(Ids.handleEntryContinueButton) {
                submitContinue()
            }
            // The machine's answer; its reason is the handle status line.
            .faunaPossible(vm.machine.handleContinuePossibility())
        }
        .padding()
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button(L.common.back) {
                    vm.machine.back()
                }
                .accessibilityIdentifier(Ids.handleEntryBackButton)
                .automationActivate(Ids.handleEntryBackButton) { vm.machine.back() }
            }
        }
    }

    /// Real "Continue" action (routing lives in the machine), referenced by both
    /// the `Button` and its `automationActivate` registration so they never diverge.
    private func submitContinue() {
        Task { _ = await vm.machine.submitHandleCheckContinue() }
    }
}
