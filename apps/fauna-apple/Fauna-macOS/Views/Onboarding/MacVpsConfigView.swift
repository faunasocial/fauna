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

                vpsUpdateChannelSection(vm: vm)

                HStack {
                    Button(L.common.back) { vm.machine.back() }
                        .accessibilityIdentifier(Ids.vpsConfigBackButton)
                        .automationActivate(Ids.vpsConfigBackButton) { vm.machine.back() }
                    Button(L.common.continue) {
                        Task { try? await vm.machine.continueFromVps() }
                    }
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier(Ids.vpsConfigContinueButton)
                    .automationActivate(Ids.vpsConfigContinueButton) {
                        Task { try? await vm.machine.continueFromVps() }
                    }
                    // Enabled and the reason are the shared answer's
                    // (`ui/README.md` § Offer only what is possible, rule 6).
                    .faunaPossible(vm.machine.vpsContinuePossibility())
                }
                FaunaPossibilityReason(vm.machine.vpsContinuePossibility())
            }
            .padding()
        }
    }
}
