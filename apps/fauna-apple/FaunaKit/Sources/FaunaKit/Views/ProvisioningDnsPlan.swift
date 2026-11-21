import SwiftUI

/// Shared DNS-plan region (`provisioning-dns-plan`) for both Apple targets
/// (iOS + macOS), rendered on `nest_provisioning` under the price recap.
///
/// Lists every record the run will keep, add, change or remove in the
/// domain's zone, one `provisioning-dns-plan-row[<name>|<type>]` per record.
/// The heading and every row's sentence come resolved from the shared
/// `OnboardingMachine::dns_plan()` snapshot, which the machine fills itself as
/// it enters the page — this view decides no action and derives no row
/// (`docs/goal/behavior/onboarding-provisioning.md` § 6 *The DNS plan*).
/// Renders nothing while the snapshot is not `visible` (the manual DNS path, a
/// bought domain's fresh zone). Mirrors the tui `wizard/nest_provisioning.rs`
/// block. The text is read-only, so it can be selected and copied
/// (`docs/goal/ui/README.md` § Text the person reads can be copied).
public struct ProvisioningDnsPlan: View {
    let plan: DnsPlanSnapshot

    public init(plan: DnsPlanSnapshot) {
        self.plan = plan
    }

    public var body: some View {
        if plan.visible {
            VStack(alignment: .leading, spacing: 2) {
                Text(renderLocalizedText(plan.heading))
                    .font(.subheadline)
                ForEach(plan.rows, id: \.key) { row in
                    automationText(
                        "\(Ids.provisioningDnsPlanRow)[\(row.key)]",
                        renderLocalizedText(row.text))
                        .font(.system(.caption, design: .monospaced))
                }
            }
            .textSelection(.enabled)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.provisioningDnsPlan)
            // A container needs its own automation read, or the in-process
            // driver reads it as absent; its text is the heading line.
            .automationValue(
                Ids.provisioningDnsPlan,
                text: { renderLocalizedText(plan.heading) })
        }
    }
}
