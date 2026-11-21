import SwiftUI
import FaunaKit

struct MacDnsConfigView: View {
    @Bindable var vm: OnboardingVM

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                automationText(Ids.pageHeading, L.onboarding.dnsConfig.title)
                    .font(.title2)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                dnsConfigFields(vm: vm)

                HStack {
                    Button(L.common.back) { vm.machine.back() }
                        .accessibilityIdentifier(Ids.dnsConfigBackButton)
                        .automationActivate(Ids.dnsConfigBackButton) { vm.machine.back() }
                    Button(L.common.continue) {
                        vm.machine.surface { try $0.continueFromDns() }
                    }
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier(Ids.dnsConfigContinueButton)
                    .automationActivate(Ids.dnsConfigContinueButton) {
                        vm.machine.surface { try $0.continueFromDns() }
                    }
                    // The machine's answer; its reason is the status line above.
                    .faunaPossible(vm.machine.dnsContinuePossibility())
                }
            }
            .padding()
        }
    }
}
