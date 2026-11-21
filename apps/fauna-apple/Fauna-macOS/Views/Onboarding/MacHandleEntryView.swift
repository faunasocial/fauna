import SwiftUI
import FaunaKit

/// handle_entry — single-page handle check + Continue routing per
/// `tests/e2e-unified/ui.yaml` and a design
/// tracked internally.
///
/// Continue is driven by `OnboardingMachine.handleCheckSnapshot()` —
/// `submit_handle_check_continue()` does the routing (DnsConfig / Done /
/// InviteRequest) so this view does not branch on outcome.
struct MacHandleEntryView: View {
    @Bindable var vm: OnboardingVM

    var body: some View {
        let snap = vm.machine.handleCheckSnapshot()
        let messageText = renderLocalizedText(snap.message)
        // One binding, used by both the TextField and `automationField` (no
        // duplication) — writes route through the same machine setter a keystroke
        // would (apple-e2e-automation.md § Resolved design point).
        let handleBinding = Binding(
            get: { vm.machine.currentHandle() },
            set: { vm.machine.setCurrentHandle(h: $0) }
        )

        VStack(alignment: .leading, spacing: 12) {
            automationText(Ids.pageHeading, L.onboarding.handle.prompt)
                .font(.title2)

            Text(L.onboarding.handle.examplesHelp)
                .textSelection(.enabled)
            Text(L.onboarding.handle.localhostHint)
                .font(.caption)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)

            HStack(spacing: 8) {
                TextField("alice@example.com", text: handleBinding)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.handleInput)
                    .automationField(Ids.handleInput, text: handleBinding)
                    .disableAutocorrection(true)

                Button(L.common.check) { vm.runHandleCheck() }
                .accessibilityIdentifier(Ids.handleCheckButton)
                .automationActivate(Ids.handleCheckButton) { vm.runHandleCheck() }
                // Painted from the shared answer (a handle typed, no check in
                // flight); `runHandleCheck` re-asks it.
                .faunaPossible(vm.handleCheckAnswer)
            }
            FaunaPossibilityReason(vm.handleCheckAnswer)

            // Auto-sizing message panel — empty when no probe has run yet.
            Text(messageText)
                .font(.callout)
                // Read-only text is selectable (ui/README.md § Text the person reads
                // can be copied); same text on a tick keeps the view, so a selection
                // survives the repaint.
                .textSelection(.enabled)
                .multilineTextAlignment(.leading)
                .frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
                .accessibilityIdentifier(Ids.handleMessageArea)
                .automationValue(Ids.handleMessageArea,
                                 text: { renderLocalizedText(vm.machine.handleCheckSnapshot().message) })

            if let error = vm.errorMessage {
                ErrorBanner(message: error)
            }

            // The by-continuing notice only while Continue signs in — the
            // shared `terms_notice_visible` (true iff `AlreadyOnNest`); the
            // other nest outcomes lead on to a page carrying its own notice.
            if snap.termsNoticeVisible {
                JoiningNestRules(summary: vm.joiningNestRules)
                TermsNotice()
            }

            HStack {
                Button(L.common.back) { vm.machine.back() }
                    .accessibilityIdentifier(Ids.handleEntryBackButton)
                    .automationActivate(Ids.handleEntryBackButton) { vm.machine.back() }

                Button(L.common.continue) { submitHandleContinue() }
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier(Ids.handleEntryContinueButton)
                .automationActivate(Ids.handleEntryContinueButton) {
                    submitHandleContinue()
                }
                // The machine's answer; its reason is the handle status line.
                .faunaPossible(vm.machine.handleContinuePossibility())
            }
        }
        .padding()
        .frame(minWidth: 480)
    }

    /// Real "Continue" action (routing lives in the machine).
    private func submitHandleContinue() {
        Task { _ = await vm.machine.submitHandleCheckContinue() }
    }
}
