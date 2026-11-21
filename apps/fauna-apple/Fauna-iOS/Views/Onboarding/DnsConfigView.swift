#if !FAUNA_KIDS
import SwiftUI
import FaunaKit

struct DnsConfigView: View {
    @Bindable var vm: OnboardingVM

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.pageHeading, L.onboarding.dnsConfig.title)
                    .font(.title2)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                dnsConfigFields(vm: vm)

                Button(L.common.continue) {
                    vm.machine.surface { try $0.continueFromDns() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .frame(maxWidth: .infinity)
                .accessibilityIdentifier(Ids.dnsConfigContinueButton)
                .automationActivate(Ids.dnsConfigContinueButton) {
                    vm.machine.surface { try $0.continueFromDns() }
                }
                // The machine's answer; its reason is the status line above.
                .faunaPossible(vm.machine.dnsContinuePossibility())
            }
            .padding()
        }
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button(L.common.back) { vm.machine.back() }
                    .accessibilityIdentifier(Ids.dnsConfigBackButton)
                    .automationActivate(Ids.dnsConfigBackButton) { vm.machine.back() }
            }
        }
    }
}
#endif // !FAUNA_KIDS
