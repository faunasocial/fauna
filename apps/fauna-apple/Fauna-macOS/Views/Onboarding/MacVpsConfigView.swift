import SwiftUI
import FaunaKit

struct MacVpsConfigView: View {
    @Bindable var vm: OnboardingVM

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                automationText(Ids.pageHeading, L.onboarding.vpsConfig.title)
                    .font(.title2)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                ProviderRow(
                    providers: PROVIDERS.filter { $0.capabilities.contains(.vps) && !$0.curatedOffers.isEmpty },
                    selectedId: vm.machine.vpsConfig().selectedProviderId,
                    kind: "vps",
                    isEnabled: { _ in true },
                    onSelect: { vm.machine.selectVpsProvider(id: $0) }
                )

                if let providerId = vm.machine.vpsConfig().selectedProviderId,
                   let provider = PROVIDERS.first(where: { $0.id == providerId }) {
                    vpsProviderSection(vm: vm, provider: provider)
                }

                HStack {
                    Button(L.common.back) { vm.machine.back() }
                        .accessibilityIdentifier(Ids.vpsConfigBackButton)
                        .automationActivate(Ids.vpsConfigBackButton) { vm.machine.back() }
                    Button(L.common.continue) {
                        Task { try? await vm.machine.continueFromVps() }
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!vm.machine.canContinueVps())
                    .accessibilityIdentifier(Ids.vpsConfigContinueButton)
                    .automationActivate(Ids.vpsConfigContinueButton,
                                        isEnabled: { vm.machine.canContinueVps() }) {
                        Task { try? await vm.machine.continueFromVps() }
                    }
                }
                // Copy comprehensibility rule 5 — why Continue is disabled
                // (`ui/README.md` § Copy comprehensibility).
                if !vm.machine.canContinueVps(), let reason = vm.machine.vpsContinueBlockedReason() {
                    Text(renderLocalizedText(reason))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .padding()
        }
    }
}
