import SwiftUI

/// recover_selfhosted_instructions — self-hosted seed install
/// (docs/goal/architecture/nest/box-recovery.md § Recovery UI (step 4), Task E).
///
/// Shared by macOS and iOS (priority #2, mirrors `AwaitingManualDnsView`). Shows
/// the installer invocation carrying `FAUNA_DEPLOYMENT_SEED` for the box
/// selected on `nest_recovery`, resolved live via `OnboardingVM
/// .resolveSelfhostedCommand()` (the reachable-nest `recoverSelfhostedCommand`
/// FFI, mirroring linux's C2 `fetch_selfhosted_command`) — a copy button, a
/// continue button, and a restore-data CTA.
///
/// Until the command resolves (no nest URL, no selection, or the fetch fails)
/// this shows the pending placeholder with copy **disabled**: an unresolved (or
/// wrong-box) command must never reach the admin's clipboard — pasting it would
/// rebuild the box under a *different* `nest_actor_id`, the exact trust break
/// recovery exists to prevent (box-recovery.md § Mechanism notes — Self-hosted
/// command).
public struct RecoverSelfhostedInstructionsView: View {
    @Bindable var vm: OnboardingVM
    @State private var resolvedCommand: String?

    public init(vm: OnboardingVM) {
        self.vm = vm
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(L.onboarding.recovery.selfhostedTitle)
                    .font(.title2)
                    .accessibilityIdentifier(Ids.pageHeading)

                Text(L.onboarding.recovery.selfhostedDesc)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                Text(resolvedCommand ?? L.onboarding.recovery.selfhostedCommandPending)
                    .font(.system(.body, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityIdentifier(Ids.recoverSelfhostedCommand)
                    .automationValue(
                        Ids.recoverSelfhostedCommand,
                        text: { resolvedCommand ?? L.onboarding.recovery.selfhostedCommandPending }
                    )

                Button(L.common.copy) { copyCommand() }
                    .disabled(resolvedCommand == nil)
                    .accessibilityIdentifier(Ids.recoverSelfhostedCopyButton)
                    .automationActivate(
                        Ids.recoverSelfhostedCopyButton,
                        isEnabled: { resolvedCommand != nil },
                        value: { resolvedCommand }
                    ) { copyCommand() }

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                HStack {
                    // Both exit the recovery flow (mirrors linux/android's shared
                    // `m.reset()`) — the box reconnects via the normal launch flow
                    // once the admin has run the installer and it is reachable.
                    Button(L.onboarding.recovery.restoreCta) { vm.recoveryReset() }
                        .accessibilityIdentifier(Ids.recoverRestoreCta)
                        .automationActivate(Ids.recoverRestoreCta) { vm.recoveryReset() }

                    Spacer()

                    Button(L.onboarding.recovery.selfhostedContinue) { vm.recoveryReset() }
                        .buttonStyle(.borderedProminent)
                        .accessibilityIdentifier(Ids.recoverSelfhostedContinueButton)
                        .automationActivate(Ids.recoverSelfhostedContinueButton) { vm.recoveryReset() }
                }
            }
            .padding()
        }
        .task {
            resolvedCommand = await vm.resolveSelfhostedCommand()
        }
    }

    private func copyCommand() {
        guard let resolvedCommand else { return }
        Pasteboard.copy(resolvedCommand)
    }
}
