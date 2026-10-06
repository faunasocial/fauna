import SwiftUI
import FaunaKit

struct MacIdentityCreatedView: View {
    @Environment(MacAppState.self) private var appState
    @Bindable var vm: OnboardingVM

    var body: some View {
        VStack(spacing: 24) {
            Image(systemName: "checkmark.shield.fill")
                .font(.system(size: 48))
                .foregroundStyle(.green)

            automationText(Ids.pageHeading, L.onboarding.identityCreated.title)
                .font(.title)

            Text(L.onboarding.identityCreated.desc)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 400)

            if let secret = vm.machine.generatedSecret() {
                VStack(alignment: .leading, spacing: 8) {
                    Text(L.onboarding.identityCreated.secretKeyLabel)
                        .font(.caption)
                        .foregroundStyle(.secondary)

                    HStack {
                        automationText(Ids.secretKeyDisplay, secret)
                            .font(.system(.body, design: .monospaced))
                            .textSelection(.enabled)
                            .padding(12)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Color(.controlBackgroundColor))
                            .clipShape(RoundedRectangle(cornerRadius: 8))
                        CopyButton(Ids.secretKeyCopyBtn, text: secret)
                    }
                }
                .frame(maxWidth: 400)
            }

            Text(L.onboarding.identityCreated.warning)
                .font(.caption)
                .foregroundStyle(.orange)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 400)

            HStack {
                Button(L.common.back) { vm.machine.back() }
                    .accessibilityIdentifier(Ids.identityCreatedBackButton)
                    .automationActivate(Ids.identityCreatedBackButton) { vm.machine.back() }

                Button(L.onboarding.identityCreated.`continue`) {
                    try? vm.confirmGeneratedIdentity(append: appState.isAddingAccount)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .accessibilityIdentifier(Ids.identityContinueButton)
                .automationActivate(Ids.identityContinueButton) { try? vm.confirmGeneratedIdentity(append: appState.isAddingAccount) }
            }
        }
    }
}
