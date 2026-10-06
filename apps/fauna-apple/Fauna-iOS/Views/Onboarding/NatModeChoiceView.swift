import SwiftUI
import FaunaKit

/// nat_mode_choice — the single, terminal admin-path setup step, reached
/// directly on a successful admin claim (Phase-4 "no-modes", S8.7).
///
/// Resolves the nest's NAT mode (public / private — the network-reachability
/// axis). `selected_mode` pre-selects the nest's seeded `node_mode` (refined
/// private-ward for a private-network target), so the common case is
/// confirm-only — one click on `nat-mode-confirm-button`. "Decide later" is
/// visible on every state and exits with the seed still in effect (already a
/// working default) — unlike the encryption defer there is no resume slot
/// and no unresolved state. No Back button — the admin is server-committed.
/// See `docs/goal/behavior/onboarding.md` § 3b-bis and
/// `tests/e2e-unified/ui.yaml` (`nat_mode_choice` page block).
struct NatModeChoiceView: View {
    @Bindable var vm: OnboardingVM

    var body: some View {
        let snap = vm.machine.natModeSnapshot()
        let publicSelected = snap.selectedMode == .public
        let inFlight = snap.state == .submitting

        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.pageHeading, L.onboarding.natMode.title)
                    .font(.title2.bold())

                Text(L.onboarding.natMode.description)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)

                RadioOptionRow(
                    id: "public-nat-mode-radio",
                    selected: publicSelected,
                    label: L.onboarding.natMode.publicLabel,
                    detail: L.onboarding.natMode.publicDesc,
                    enabled: !inFlight
                ) { vm.machine.selectNatMode(mode: .public) }

                RadioOptionRow(
                    id: "private-nat-mode-radio",
                    selected: !publicSelected,
                    label: L.onboarding.natMode.privateLabel,
                    detail: L.onboarding.natMode.privateDesc,
                    enabled: !inFlight
                ) { vm.machine.selectNatMode(mode: .private) }

                Text(renderLocalizedText(snap.message))
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityIdentifier(Ids.natModeStatus)
                    .automationValue(Ids.natModeStatus,
                                     text: { renderLocalizedText(vm.machine.natModeSnapshot().message) })

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                Button(L.onboarding.natMode.confirmButton) { submitMode() }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .frame(maxWidth: .infinity)
                .disabled(!snap.submitEnabled)
                .accessibilityIdentifier(Ids.natModeConfirmButton)
                .automationActivate(Ids.natModeConfirmButton,
                                    isEnabled: { vm.machine.natModeSnapshot().submitEnabled }) {
                    submitMode()
                }

                Button(L.onboarding.natMode.deferButton) {
                    _ = vm.machine.deferNatModeChoice()
                }
                .buttonStyle(.bordered)
                .controlSize(.large)
                .frame(maxWidth: .infinity)
                .accessibilityIdentifier(Ids.natModeDeferButton)
                .automationActivate(Ids.natModeDeferButton) {
                    _ = vm.machine.deferNatModeChoice()
                }
            }
            .padding()
        }
    }

    /// The real confirm action, referenced by both the confirm Button and its
    /// automation registration so they never diverge (apple-e2e-automation.md
    /// § Resolved design point — same-symbol convention).
    private func submitMode() {
        Task { _ = await vm.machine.submitNatModeChoice() }
    }
}
