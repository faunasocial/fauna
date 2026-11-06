import SwiftUI

/// Stage `emergency_lockout_entry`: the signed-out lock door
/// (`docs/goal/ui/sessions.md` § Layout & flow → *The signed-out door*;
/// `devices.md` § The signed-out door), reached from `identity_choice`'s
/// `emergency-lockout-entry-button` — paste the secret key, give the handle,
/// type `LOCK`. Pre-identity; persists nothing (the three inputs live in this
/// view's `@State` only).
///
/// Submit runs the shared machine's `submit_emergency_lockout` through
/// `OnboardingVM.submitEmergencyLockout`: a landed lock returns the wizard to
/// `identity_choice` with the confirmation on its `error-message`; every refusal
/// stays here and speaks — the machine's one table
/// (`emergency_lockout_outcome_message`). The warning is the signed-in Sessions
/// page's REQUIRED copy, one string for both. Shared by macOS and iOS (priority
/// #2); twin of tui's and linux's `emergency_lockout_entry` and android's
/// `EmergencyLockoutEntryScreen`.
public struct EmergencyLockoutEntryView: View {
    @Bindable var vm: OnboardingVM
    @State private var secret = ""
    @State private var account = ""
    @State private var confirm = ""
    @State private var submitting = false
    /// The shared word: gates the button here AND in the machine.
    private let confirmWord = sessionsLockoutConfirmWord()

    public init(vm: OnboardingVM) {
        self.vm = vm
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Text(L.onboarding.emergencyLockout.title)
                    .font(.title2.bold())
                Text(L.onboarding.emergencyLockout.desc)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Text(L.onboarding.emergencyLockout.accountHint)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                Text(L.onboarding.emergencyLockout.secretLabel)
                TextField(L.onboarding.emergencyLockout.secretLabel, text: $secret)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(.body, design: .monospaced))
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    #endif
                    .accessibilityIdentifier(Ids.pasteSecretField)
                    .automationField(Ids.pasteSecretField, text: $secret)

                Text(L.common.handle)
                TextField(L.common.handle, text: $account)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    #endif
                    .accessibilityIdentifier(Ids.recoveryEntryAccountField)
                    .automationField(Ids.recoveryEntryAccountField, text: $account)

                automationText(Ids.sessionsLockoutWarning, L.sessions.lockoutWarning)
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
                TextField(L.sessions.lockoutConfirmPlaceholder, text: $confirm)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.characters)
                    #endif
                    .accessibilityIdentifier(Ids.sessionsLockoutConfirmField)
                    .automationField(Ids.sessionsLockoutConfirmField, text: $confirm)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                HStack(spacing: 12) {
                    Button(L.common.back) { vm.machine.back() }
                        .controlSize(.large)
                        .disabled(submitting)
                        .accessibilityIdentifier(Ids.emergencyLockoutBackButton)
                        .automationActivate(Ids.emergencyLockoutBackButton, isEnabled: { !submitting }) {
                            vm.machine.back()
                        }
                    Spacer()
                    Button(L.sessions.lockoutButton, role: .destructive) { submit() }
                        .buttonStyle(.borderedProminent)
                        .tint(.red)
                        .controlSize(.large)
                        .disabled(!enabled)
                        .accessibilityIdentifier(Ids.sessionsLockoutButton)
                        .automationActivate(Ids.sessionsLockoutButton, isEnabled: { enabled }) { submit() }
                }
            }
            .padding()
        }
    }

    private var enabled: Bool { !submitting && confirm == confirmWord }

    /// Referenced by both the `Button` and its `automationActivate` so the two
    /// never diverge (apple-e2e-automation.md § Resolved design point).
    private func submit() {
        guard !submitting else { return }
        submitting = true
        let secret = secret
        let account = account
        let confirm = confirm
        Task {
            await vm.submitEmergencyLockout(secret: secret, account: account, confirmWord: confirm)
            submitting = false
        }
    }
}
