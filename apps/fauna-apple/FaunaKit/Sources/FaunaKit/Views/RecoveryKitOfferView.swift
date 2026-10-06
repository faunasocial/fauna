import SwiftUI

/// Stage `recovery_kit`: the recovery-kit offer right after the identity secret
/// is confirmed (`onboarding.md` § 1 Identity) — apple's leg of the screen tui
/// led (reference impls: tui `wizard/recovery_kit.rs`, linux
/// `views/onboarding/recovery_kit.rs`).
///
/// The page **mints and displays only**: no nest exists at this position, so
/// registration + escrow run at the signed-in handoff (`OnboardingVM`
/// latches `takePendingRecoverySecret()` onto
/// `SessionState.pendingRecoveryKitHex`; `MailEnableGlue
/// .registerPendingRecoveryKit` registers THAT root). That is why
/// `recovery-kit-escrow-status` renders exactly one state here — the deferred
/// line, never "protected". The machine routes here only because
/// `OnboardingVM.init` declares `setRendersRecoveryKit(true)`.
///
/// The display is the bare 64-hex (what a user copies onto paper); the QR and
/// the copy button both carry the machine's one `fauna://recovery` URI
/// (`identity-succession.md` § The RecoveryKey — *Which encoding each
/// affordance carries*). Same ids and shape as Settings' shown-once kit
/// (`RecoveryKitSection.mintedKitDisplay`) — the same artifact, shown the same
/// way (priority #3).
///
/// Shared by macOS and iOS (priority #2), mirroring `TrustPromptView`.
public struct RecoveryKitOfferView: View {
    @Bindable var vm: OnboardingVM

    public init(vm: OnboardingVM) {
        self.vm = vm
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                // Untagged, like tui/linux: the page's approved id set has no
                // `page-heading`.
                Text(L.onboarding.recoveryKit.title)
                    .font(.title2.bold())

                automationText(Ids.recoveryKitDescription, L.onboarding.recoveryKit.desc)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)

                automationText(
                    Ids.recoveryKitSecretDisplay,
                    vm.recoveryKitSecretHex ?? L.onboarding.recoveryKit.notMinted
                )
                .font(.body.monospaced())
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)

                Button(L.common.copy) { vm.copyRecoveryKit() }
                    .disabled(vm.recoveryKitUri == nil)
                    .accessibilityIdentifier(Ids.recoveryKitSecretCopyBtn)
                    .automationActivate(
                        Ids.recoveryKitSecretCopyBtn,
                        isEnabled: { vm.recoveryKitUri != nil }
                    ) {
                        vm.copyRecoveryKit()
                    }

                if let uri = vm.recoveryKitUri, let matrix = try? qrMatrix(data: uri) {
                    QrCodeView(matrix: matrix)
                        .frame(width: 200, height: 200)
                        .accessibilityIdentifier(Ids.recoveryKitQr)
                        .automationValue(Ids.recoveryKitQr, text: { "\(matrix.size)" })
                }

                automationText(Ids.recoveryKitEscrowStatus, L.onboarding.recoveryKit.escrowDeferred)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                HStack(spacing: 12) {
                    // Skip is one click and never blocks onboarding; the minted
                    // root is dropped, so nothing registers at handoff and
                    // Settings' never-created warning tells the truth.
                    Button(L.onboarding.recoveryKit.skip) { vm.machine.skipRecoveryKit() }
                        .buttonStyle(.bordered)
                        .controlSize(.large)
                        .accessibilityIdentifier(Ids.recoveryKitSkipButton)
                        .automationActivate(Ids.recoveryKitSkipButton) {
                            vm.machine.skipRecoveryKit()
                        }

                    Button(L.onboarding.recoveryKit.confirm) { vm.machine.confirmRecoveryKit() }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.large)
                        .disabled(vm.recoveryKitSecretHex == nil)
                        .accessibilityIdentifier(Ids.recoveryKitConfirmButton)
                        .automationActivate(
                            Ids.recoveryKitConfirmButton,
                            isEnabled: { vm.recoveryKitSecretHex != nil }
                        ) {
                            vm.machine.confirmRecoveryKit()
                        }
                }
                .frame(maxWidth: .infinity, alignment: .trailing)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
        }
    }
}
