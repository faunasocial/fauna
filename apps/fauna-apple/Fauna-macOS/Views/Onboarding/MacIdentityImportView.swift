import SwiftUI
import FaunaKit

struct MacIdentityImportView: View {
    @Environment(MacAppState.self) private var appState
    @Bindable var vm: OnboardingVM
    @State private var importSecret: String = ""

    var body: some View {
        VStack(spacing: 24) {
            automationText(Ids.pageHeading, L.onboarding.identityImport.title)
                .font(.title)

            Text(L.onboarding.identityImport.pasteSubtitle)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            VStack(spacing: 12) {
                TextField(L.onboarding.identityImport.pastePlaceholder, text: $importSecret)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(.body, design: .monospaced))
                    .frame(maxWidth: 400)
                    .accessibilityIdentifier(Ids.pasteSecretField)
                    .automationField(Ids.pasteSecretField, text: $importSecret)

                Button(L.onboarding.identityImport.`import`) {
                    submitImport()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(importSecret.isEmpty)
                .accessibilityIdentifier(Ids.importSubmitButton)
                .automationActivate(Ids.importSubmitButton, isEnabled: { !importSecret.isEmpty }) {
                    submitImport()
                }
            }

            if let error = vm.errorMessage {
                ErrorBanner(message: error)
            }

            Button(L.common.back) {
                vm.machine.back()
            }
            .controlSize(.small)
            .accessibilityIdentifier(Ids.identityImportBackButton)
            .automationActivate(Ids.identityImportBackButton) { vm.machine.back() }
        }
    }

    /// The real "Import" action, referenced by both the `Button` and the
    /// `automationActivate` registration so the two never diverge (the
    /// registration-ergonomics convention — apple-e2e-automation.md § Resolved
    /// design point). Routes through the shared `vm.importIdentity` parser so a
    /// pasted `fauna://identity?secret=&handle=` URI works (and pre-fills the
    /// handle) on macOS too — matching iOS/web/android (priority #1/#4).
    private func submitImport() {
        vm.importIdentity(importSecret, append: appState.isAddingAccount)
    }
}
