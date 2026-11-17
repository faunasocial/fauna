#if !FAUNA_KIDS
import SwiftUI
import FaunaKit

struct VpsConfigView: View {
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

                Button(L.common.continue) {
                    Task { try? await vm.machine.continueFromVps() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .frame(maxWidth: .infinity)
                .accessibilityIdentifier(Ids.vpsConfigContinueButton)
                .automationActivate(Ids.vpsConfigContinueButton) {
                    Task { try? await vm.machine.continueFromVps() }
                }
                // Enabled and the reason are the shared answer's
                // (`ui/README.md` § Offer only what is possible, rule 6).
                .faunaPossible(vm.machine.vpsContinuePossibility())
                FaunaPossibilityReason(vm.machine.vpsContinuePossibility())
            }
            .padding()
        }
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button(L.common.back) { vm.machine.back() }
                    .accessibilityIdentifier(Ids.vpsConfigBackButton)
                    .automationActivate(Ids.vpsConfigBackButton) { vm.machine.back() }
            }
        }
    }
}
#endif // !FAUNA_KIDS
