import SwiftUI
import FaunaKit

struct IdentityCreatedView: View {
    @Environment(AppState.self) private var appState
    @Bindable var vm: OnboardingVM

    var body: some View {
        VStack(spacing: 24) {
            Spacer()

            Image(systemName: "checkmark.shield.fill")
                .font(.system(size: 56))
                .foregroundStyle(.green)

            Text(L.onboarding.identityCreated.title)
                .font(.title2.bold())

            Text(L.onboarding.identityCreated.desc)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 24)

            if let secret = vm.machine.generatedSecret() {
                VStack(alignment: .leading, spacing: 8) {
                    Text(L.onboarding.identityCreated.secretKeyLabel)
                        .font(.caption)
                        .foregroundStyle(.secondary)

                    HStack(alignment: .top, spacing: 8) {
                        automationText(Ids.secretKeyDisplay, secret)
                            .font(.system(.body, design: .monospaced))
                            .padding(12)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Color(.secondarySystemBackground))
                            .clipShape(RoundedRectangle(cornerRadius: 8))

                        CopyButton(Ids.secretKeyCopyBtn, text: secret)
                            .padding(.top, 12)
                    }
                }
                .padding(.horizontal, 24)
            }

            Text(L.onboarding.identityCreated.warning)
                .font(.caption)
                .foregroundStyle(.orange)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)

            Spacer()

            Button(L.onboarding.identityCreated.`continue`) {
                try? vm.confirmGeneratedIdentity(append: appState.isAddingAccount)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .accessibilityIdentifier(Ids.identityContinueButton)
            .automationActivate(Ids.identityContinueButton) { try? vm.confirmGeneratedIdentity(append: appState.isAddingAccount) }
        }
        .padding()
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button(L.common.back) {
                    vm.machine.back()
                }
                .accessibilityIdentifier(Ids.identityCreatedBackButton)
                .automationActivate(Ids.identityCreatedBackButton) { vm.machine.back() }
            }
        }
    }
}
