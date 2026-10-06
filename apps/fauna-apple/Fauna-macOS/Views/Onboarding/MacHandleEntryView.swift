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
        let handle = vm.machine.currentHandle()
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
            Text(L.onboarding.handle.localhostHint)
                .font(.caption)
                .foregroundStyle(.secondary)

            HStack(spacing: 8) {
                TextField("alice@example.com", text: handleBinding)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.handleInput)
                    .automationField(Ids.handleInput, text: handleBinding)
                    .disableAutocorrection(true)

                Button(L.common.check) { runHandleCheck() }
                .disabled(handle.isEmpty)
                .accessibilityIdentifier(Ids.handleCheckButton)
                .automationActivate(Ids.handleCheckButton,
                                    isEnabled: { !vm.machine.currentHandle().isEmpty }) {
                    runHandleCheck()
                }
            }

            // Auto-sizing message panel — empty when no probe has run yet.
            Text(messageText)
                .font(.callout)
                .multilineTextAlignment(.leading)
                .frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
                .accessibilityIdentifier(Ids.handleMessageArea)
                .automationValue(Ids.handleMessageArea,
                                 text: { renderLocalizedText(vm.machine.handleCheckSnapshot().message) })

            if snap.controlCheckboxVisible {
                Toggle(L.onboarding.handle.controlCheckbox, isOn: Binding(
                    get: { snap.controlCheckboxChecked },
                    set: { vm.machine.setControlCheckbox(checked: $0) }
                ))
                    .accessibilityIdentifier(Ids.handleControlCheckbox)
                    // A toggle's "click" is a flip — read the live checked state
                    // and invert it through the same machine setter.
                    .automationActivate(Ids.handleControlCheckbox) {
                        vm.machine.setControlCheckbox(
                            checked: !vm.machine.handleCheckSnapshot().controlCheckboxChecked)
                    }
            }

            if let error = vm.errorMessage {
                ErrorBanner(message: error)
            }

            HStack {
                Button(L.common.back) { vm.machine.back() }
                    .accessibilityIdentifier(Ids.handleEntryBackButton)
                    .automationActivate(Ids.handleEntryBackButton) { vm.machine.back() }

                Button(L.common.continue) { submitHandleContinue() }
                .keyboardShortcut(.defaultAction)
                .disabled(!snap.continueEnabled)
                .accessibilityIdentifier(Ids.handleEntryContinueButton)
                .automationActivate(Ids.handleEntryContinueButton,
                                    isEnabled: { vm.machine.handleCheckSnapshot().continueEnabled }) {
                    submitHandleContinue()
                }
            }
        }
        .padding()
        .frame(minWidth: 480)
    }

    /// Real "Check" action, referenced by both the `Button` and its
    /// `automationActivate` registration so they never diverge.
    private func runHandleCheck() {
        Task { await vm.machine.startHandleCheck(handle: vm.machine.currentHandle()) }
    }

    /// Real "Continue" action (routing lives in the machine).
    private func submitHandleContinue() {
        Task { _ = await vm.machine.submitHandleCheckContinue() }
    }
}
