import SwiftUI
import FaunaKit

struct OnboardingContainerView: View {
    @Environment(MacAppState.self) private var appState
    @Bindable var vm: OnboardingVM
    @State private var doneExit: OnboardingVM.OnboardingExit = .unknown

    var body: some View {
        Group {
            switch vm.step {
            case .identityChoice:
                // Shared FaunaKit view (priority #2, mirrors `TrustPromptView`).
                IdentityChoiceView(vm: vm)
            case .identityCreated:
                MacIdentityCreatedView(vm: vm)
            case .identityImport:
                MacIdentityImportView(vm: vm)
            case .handleEntry:
                MacHandleEntryView(vm: vm)
            case .dnsConfig:
                MacDnsConfigView(vm: vm)
            case .vpsConfig:
                MacVpsConfigView(vm: vm)
            case .nestProvisioning:
                MacNestProvisioningView(vm: vm)
            case .dnsPostInstructions:
                MacDnsPostInstructionsView(vm: vm)
            case .inviteRequest:
                MacInviteRequestView(vm: vm)
            // claim_code on an unclaimed nest, then the terminal admin-path
            // NAT-mode confirm step (docs/goal/behavior/onboarding.md §3b-bis).
            case .claimCode:
                MacClaimCodeView(vm: vm)
            case .natModeChoice:
                MacNatModeChoiceView(vm: vm)
            // Box-recovery step-4 recovery-wizard steps (docs/goal/architecture/nest/
            // box-recovery.md § Recovery UI, Task E) — shared FaunaKit views
            // (priority #2, mirrors `AwaitingManualDnsView`).
            case .nestRecovery:
                NestRecoveryView(vm: vm)
            case .recoverSelfhostedInstructions:
                RecoverSelfhostedInstructionsView(vm: vm)
            // Identity-recovery onboarding steps (onboarding.md § 1 Identity) —
            // shared FaunaKit views; `setRendersRecoveryKit(true)` declared at
            // `OnboardingVM.init`, the restore reached via
            // `restore-from-recovery-kit-button` on `IdentityChoiceView`.
            case .recoveryKit:
                RecoveryKitOfferView(vm: vm)
            case .recoveryEntry:
                RecoveryEntryView(vm: vm, isAddingAccount: { appState.isAddingAccount })
            // The one-tap trust offer (onboarding.md § 3b-ter) — shared
            // FaunaKit view, `setRendersTrustPrompt(true)` declared at
            // `OnboardingVM.init`.
            case .trustPrompt:
                TrustPromptView(vm: vm)
            case .done:
                doneRouter
            }
        }
        .padding(40)
    }

    @ViewBuilder
    private var doneRouter: some View {
        switch doneExit {
        case .feed:
            // Already handed off — onboarding window closes after the
            // app router swaps to the authenticated UI.
            Color.clear
        // ⚠ There is deliberately no `.inviteSubmitted` arm (retired 2026-08-12).
        // It rendered a dead-end placeholder under an UNDECLARED test id
        // (`invite-submitted-placeholder` — a rule-A deviation), and that whole
        // exit is gone: the pending-review journey stays on `invite_request` and
        // polls (`onboarding.md` § The pending-invite surface).
        case .awaitingManualDns:
            AwaitingManualDnsView(vm: vm) {
                // The claim advanced the machine off AwaitingManualDns. Reset the
                // router so the container re-evaluates: the machine now sits at a real
                // step, rendered by the top-level `switch vm.step` — NatModeChoice on a
                // fresh claim, or the trust offer (`.trustPrompt`, onboarding.md
                // § 3b-ter) on the already-claimed recovery edge — and reaches `.done`
                // again only at `LoggedIn`, where the `.unknown` arm re-runs the
                // handoff → `.feed`. The awaiting slot stays until that handoff.
                doneExit = .unknown
            }
        case .unknown:
            Color.clear.onAppear {
                // First entry to the .done step — run the handoff once.
                doneExit = vm.completeOnboarding(
                    sessionState: appState.session, append: appState.isAddingAccount)
                if case .feed = doneExit {
                    // A new session lands where a fresh launch does — never on
                    // the page the previous session left open (see
                    // `MacAppState.landingSidebar`).
                    appState.selectedSidebar = MacAppState.landingSidebar
                    // Build the authed FaunaClient via the same launch gate a
                    // returning user hits — `performLoggedInHandoff` just persisted
                    // full identity to the keychain, so `runLaunch()` routes it through
                    // the shared `LaunchMachine` (landing on `LaunchPhase.online`) to
                    // `completeAuthenticatedLaunch`, which sets `isOnboarded` AND
                    // fires the first-setup mail/caldav glue. Falls back to a bare
                    // `isOnboarded` flip if the App never wired the hook.
                    if let onLaunch = appState.onLaunchAuthenticated {
                        onLaunch()
                    } else {
                        appState.isOnboarded = true
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func wizardExitPlaceholder(titleKey: String, a11yId: String) -> some View {
        VStack(spacing: 16) {
            Text(L.lookup(titleKey))
                .font(.title3)
                .multilineTextAlignment(.center)
                .accessibilityIdentifier(a11yId)
            Text(L.common.dismiss)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding()
        .frame(minWidth: 480)
    }
}
