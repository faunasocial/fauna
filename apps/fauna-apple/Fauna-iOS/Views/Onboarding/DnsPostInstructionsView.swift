import SwiftUI
import FaunaKit

struct DnsPostInstructionsView: View {
    @Bindable var vm: OnboardingVM

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text(L.onboarding.dnsPostInstructions.title)
                    .font(.title2)
                    .accessibilityIdentifier(Ids.pageHeading)

                Text(L.onboarding.dnsPostInstructions.description)

                ScrollView {
                    // Dynamic label: re-reads `dnsPostInstructions()` on every
                    // registry lookup, so the polled text reflects the seeded
                    // records once the page's first render lands.
                    automationText(Ids.dnsPostInstructionsText, vm.machine.dnsPostInstructions() ?? "")
                        .font(.system(.body, design: .monospaced))
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(minHeight: 200)

                Button(L.onboarding.dnsPostInstructions.copyButton) { vm.copyDnsPostInstructions() }
                .buttonStyle(.bordered)
                .accessibilityIdentifier(Ids.dnsPostInstructionsCopyButton)
                .automationActivate(Ids.dnsPostInstructionsCopyButton) { vm.copyDnsPostInstructions() }

                Button(L.common.continue) {
                    _ = vm.machine.continueFromDnsPostInstructions()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .frame(maxWidth: .infinity)
                .accessibilityIdentifier(Ids.dnsPostInstructionsContinueButton)
                .automationActivate(Ids.dnsPostInstructionsContinueButton) {
                    _ = vm.machine.continueFromDnsPostInstructions()
                }
            }
            .padding()
        }
    }
}
