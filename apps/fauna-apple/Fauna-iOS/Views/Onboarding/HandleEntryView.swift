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
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity, alignment: .center)

            Text(L.onboarding.handle.localhostHint)
                .font(.caption)
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
                .onSubmit { runCheck() }

                Button(L.common.check) { runCheck() }
                    .buttonStyle(.bordered)
                    .disabled(vm.machine.currentHandle().isEmpty)
                    .accessibilityIdentifier(Ids.handleCheckButton)
                    .automationActivate(Ids.handleCheckButton,
                                        isEnabled: { !vm.machine.currentHandle().isEmpty }) {
                        runCheck()
                    }
            }

            Text(renderLocalizedText(snap.message))
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityIdentifier(Ids.handleMessageArea)
                .automationValue(Ids.handleMessageArea,
                                 text: { renderLocalizedText(vm.machine.handleCheckSnapshot().message) })

            if snap.controlCheckboxVisible {
                // Use a Button wrapping the Toggle so the entire row is the
                // tap target (XCUITest's `tap()` on a SwiftUI Toggle's
                // accessibility wrapper doesn't reliably flip the underlying
                // UISwitch on iOS — the inner Switch is hit-testable only on
                // its right edge).
                Button(action: {
                    vm.machine.setControlCheckbox(checked: !snap.controlCheckboxChecked)
                }) {
                    HStack {
                        Image(systemName: snap.controlCheckboxChecked
                              ? "checkmark.square.fill" : "square")
                            .foregroundStyle(snap.controlCheckboxChecked
                                             ? Color.accentColor : Color.secondary)
                        Text(L.onboarding.handle.controlCheckbox)
                            .foregroundStyle(.primary)
                        Spacer()
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier(Ids.handleControlCheckbox)
                // A toggle's "click" is a flip — read the live checked state and
                // invert it through the same machine setter the Button action uses.
                .automationActivate(Ids.handleControlCheckbox) {
                    vm.machine.setControlCheckbox(
                        checked: !vm.machine.handleCheckSnapshot().controlCheckboxChecked)
                }
            }

            if let error = vm.errorMessage {
                ErrorBanner(message: error)
            }

            Button(L.common.continue) { submitContinue() }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .frame(maxWidth: .infinity)
            .disabled(!snap.continueEnabled)
            .accessibilityIdentifier(Ids.handleEntryContinueButton)
            .automationActivate(Ids.handleEntryContinueButton,
                                isEnabled: { vm.machine.handleCheckSnapshot().continueEnabled }) {
                submitContinue()
            }
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

    private func runCheck() {
        let handle = vm.machine.currentHandle()
        guard !handle.isEmpty else { return }
        Task { await vm.machine.startHandleCheck(handle: handle) }
    }

    /// Real "Continue" action (routing lives in the machine), referenced by both
    /// the `Button` and its `automationActivate` registration so they never diverge.
    private func submitContinue() {
        Task { _ = await vm.machine.submitHandleCheckContinue() }
    }
}
