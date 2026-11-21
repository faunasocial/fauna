import SwiftUI

/// Stage `recovery_entry`: the phrase-only identity restore (`onboarding.md`
/// § 1 Identity), reached from `identity_choice`'s
/// `restore-from-recovery-kit-button` — apple's leg of the page tui led
/// (reference impls: tui `wizard/recovery_entry.rs`, linux
/// `views/onboarding/recovery_entry.rs`).
///
/// Submit runs the shared machine's pre-identity escrow restore through
/// `OnboardingVM.submitRecoveryEntry`, which folds the answer back: a restored
/// seed lands on `handle_entry` committed exactly like an import; every refusal
/// is the shared `recoveryEntryOutcomeMessage` table on `error-message`;
/// `superseded` routes to the import screen instead of speaking. The account
/// field asks for a handle (`user@domain`, or the nest's own address after the
/// `@` when the domain is gone) — the ceremony has no session to ask where the
/// account lives. `qr-camera-view` (optional on this page) is not rendered
/// here: a paste is the path on both apple platforms today, as on
/// `identity_import`.
///
/// Shared by macOS and iOS (priority #2), mirroring `TrustPromptView`.
public struct RecoveryEntryView: View {
    @Bindable var vm: OnboardingVM
    /// The caller's LIVE `appState.isAddingAccount`, read at submit time —
    /// never a value captured at body evaluation (`OnboardingVM
    /// .confirmGeneratedIdentity`'s doc has the why).
    private let isAddingAccount: () -> Bool
    @State private var phrase: String = ""
    @State private var account: String = ""
    @State private var submitting = false

    public init(vm: OnboardingVM, isAddingAccount: @escaping () -> Bool) {
        self.vm = vm
        self.isAddingAccount = isAddingAccount
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(L.onboarding.recoveryEntry.title)
                    .font(.title2.bold())

                Text(L.onboarding.recoveryEntry.desc)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Text(L.onboarding.recoveryEntry.accountHint)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                Text(L.onboarding.recoveryEntry.phraseLabel)
                TextField(L.onboarding.recoveryEntry.phraseLabel, text: $phrase)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(.body, design: .monospaced))
                    .autocorrectionDisabled()
                    .accessibilityIdentifier(Ids.recoveryEntryPhraseField)
                    .automationField(Ids.recoveryEntryPhraseField, text: $phrase)

                // The shared `common.handle` label: the same field concept as
                // `handle_entry`'s (priority #3), which a restore pre-fills.
                Text(L.common.handle)
                TextField(L.common.handle, text: $account)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    #endif
                    .accessibilityIdentifier(Ids.recoveryEntryAccountField)
                    .automationField(Ids.recoveryEntryAccountField, text: $account)

                HStack(spacing: 12) {
                    Button(L.common.back) { vm.machine.back() }
                        .controlSize(.large)
                        .accessibilityIdentifier(Ids.recoveryEntryBackButton)
                        .automationActivate(Ids.recoveryEntryBackButton) { vm.machine.back() }

                    // Painted from the shared draft answer (a kit, then an
                    // account — `recovery_entry_possibility`), its reason
                    // within eyeshot below, and blocked while a restore is in
                    // flight. The machine's `submit_recovery_entry` runs the
                    // same checks, so the press asks what the paint asked.
                    Button(L.onboarding.recoveryEntry.submit) { submit() }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.large)
                        .accessibilityIdentifier(Ids.recoveryEntrySubmitButton)
                        .automationActivate(Ids.recoveryEntrySubmitButton) { submit() }
                        .faunaPossible(answer)
                }
                .frame(maxWidth: .infinity, alignment: .trailing)
                FaunaPossibilityReason(answer)
                    .frame(maxWidth: .infinity, alignment: .trailing)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
        }
    }

    /// `recovery-entry-submit-button`'s one answer
    /// (``OnboardingVM/recoveryEntryAnswer(phrase:account:submitting:)``).
    private var answer: Possibility {
        vm.recoveryEntryAnswer(phrase: phrase, account: account, submitting: submitting)
    }

    /// Referenced by both the `Button` and its `automationActivate` so the two
    /// never diverge (apple-e2e-automation.md § Resolved design point).
    private func submit() {
        guard !submitting else { return }
        submitting = true
        let phrase = phrase
        let account = account
        let append = isAddingAccount()
        Task {
            await vm.submitRecoveryEntry(phrase: phrase, account: account, append: append)
            submitting = false
        }
    }
}
