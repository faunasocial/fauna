import SwiftUI
import FaunaKit

struct WelcomeView: View {
    @Bindable var vm: OnboardingVM
    @Environment(AppState.self) private var appState
    @State private var doneExit: OnboardingVM.OnboardingExit = .unknown

    var body: some View {
        NavigationStack {
            Group {
                switch vm.step {
                case .identityChoice:
                    // Shared FaunaKit view (priority #2, mirrors `TrustPromptView`).
                    IdentityChoiceView(vm: vm)
                case .identityCreated:
                    IdentityCreatedView(vm: vm)
                case .identityImport:
                    IdentityImportView(vm: vm)
                case .handleEntry:
                    HandleEntryView(vm: vm)
                case .dnsConfig:
                    DnsConfigView(vm: vm)
                case .vpsConfig:
                    VpsConfigView(vm: vm)
                case .nestProvisioning:
                    NestProvisioningView(vm: vm)
                case .dnsPostInstructions:
                    DnsPostInstructionsView(vm: vm)
                case .inviteRequest:
                    InviteRequestView(vm: vm)
                // claim_code on an unclaimed nest, then the terminal admin-path
                // NAT-mode confirm step (docs/goal/behavior/onboarding.md §3b-bis).
                case .claimCode:
                    ClaimCodeView(vm: vm)
                case .natModeChoice:
                    NatModeChoiceView(vm: vm)
                // Box-recovery step-4 recovery-wizard steps (box-recovery.md § Recovery
                // UI, Task E) — shared FaunaKit views (priority #2, mirrors
                // `AwaitingManualDnsView`). Mirrors `OnboardingContainerView` (macOS).
                case .nestRecovery:
                    NestRecoveryView(vm: vm)
                case .recoverSelfhostedInstructions:
                    RecoverSelfhostedInstructionsView(vm: vm)
                // Identity-recovery onboarding steps (onboarding.md § 1
                // Identity) — shared FaunaKit views, mirroring
                // `OnboardingContainerView` (macOS).
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
        }
    }

    /// Routes the wizard's `.done` step on `wizardOutcome()` per
    /// docs/goal/behavior/onboarding.md §"Wizard exit handling". Mirrors macOS's
    /// `OnboardingContainerView.doneRouter`: the `.unknown` arm runs the handoff
    /// (which persists any pending / awaiting-DNS slot) exactly once, and the
    /// resulting exit selects the surface. LoggedIn enters the authenticated UI;
    /// AwaitingManualDns renders the "Almost ready" surface. The pending-invite
    /// journey never reaches here at all — it has no wizard exit.
    @ViewBuilder
    private var doneRouter: some View {
        switch doneExit {
        case .feed:
            // Handed off — the onboarding view is replaced by the authenticated UI.
            Color.clear
        // ⚠ There is deliberately no `.inviteSubmitted` arm (retired 2026-08-12).
        // It rendered `Color.clear` — nothing at all — and that whole exit is
        // gone: the pending-review journey stays on `invite_request` and polls
        // (`onboarding.md` § The pending-invite surface).
        case .awaitingManualDns:
            AwaitingManualDnsView(vm: vm) {
                // The claim advanced the machine off AwaitingManualDns. Reset the
                // router so the container re-evaluates: the machine now sits at a real
                // step, rendered by `switch vm.step` — NatModeChoice on a fresh claim,
                // or the trust offer (`.trustPrompt`, onboarding.md § 3b-ter) on the
                // already-claimed recovery edge — and reaches `.done` again only at
                // `LoggedIn`, where the `.unknown` arm re-runs the handoff → `.feed`.
                // The awaiting slot stays until that handoff.
                doneExit = .unknown
            }
        case .unknown:
            Color.clear.onAppear { runHandoff() }
        }
    }

    private func runHandoff() {
        let exit = vm.completeOnboarding(
            sessionState: appState.session, append: appState.isAddingAccount)
        doneExit = exit
        if case .feed = exit {
            // Build the authed FaunaClient via the same launch path a returning
            // user hits — `performLoggedInHandoff` just persisted full identity to
            // the keychain, so `runLaunch()` routes it through the shared
            // `LaunchMachine` (landing on `LaunchPhase.online`), which builds the
            // client, clears `isOnboarding`, AND fires the first-setup mail/caldav
            // glue. Falls back to a bare `isOnboarding` flip if the App never wired
            // the hook.
            if let onLaunch = appState.onLaunchAuthenticated {
                onLaunch()
            } else {
                appState.isOnboarding = false
            }
        }
    }

}
