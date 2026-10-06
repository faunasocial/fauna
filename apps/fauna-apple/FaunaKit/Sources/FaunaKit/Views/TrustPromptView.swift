import SwiftUI

/// Stage 3b-ter: the one-tap "trust this box" offer (`onboarding.md`
/// § 3b-ter) — the one ratified survivor of the retired claim-time trust
/// question (`storage-modes.md` § What replaced each piece of the axis).
///
/// ui.yaml `onboarding.trust_prompt` elements: `trust-box-summary`,
/// `trust-box-grant-button`, `trust-box-skip-button` (+ `error-message`).
/// Deliberately **no `page-heading`** — the approved set is exactly those
/// three (tui, the lead app, and linux both render an untagged title above
/// the summary for readability; this view does the same).
///
/// **No Back button** — like the NAT page before it, the admin is already
/// server-committed by the time this shows; grant and skip are its only exits
/// and both conclude the wizard with the same `LoggedIn` outcome.
///
/// The screen **asks only.** Minting the default set needs an authenticated
/// session and the nest's content-processor roster, neither of which the
/// wizard holds, so the answer is latched (`grantDefaultTrust` /
/// `skipTrustPrompt` → `takeTrustPromptGranted`) and the mint runs at the
/// signed-in handoff (`OnboardingVM.performLoggedInHandoff` latches the
/// answer onto `SessionState.pendingTrustPromptGranted`; the post-auth
/// launch glue `MailEnableGlue.applyPendingTrustPromptGrant` consumes it) —
/// the same deferral the deployment seed and DNS credential use. apple
/// reaches this page only because `OnboardingVM.init` declares
/// `setRendersTrustPrompt(true)`; before that it exited `natModeChoice`
/// straight to `Done`.
///
/// ⚠ **The click handlers must ONLY latch, never conclude the wizard
/// inline** — measured on linux (`test_trust_prompt.py`, 2026-08-14): both
/// answers are synchronous local latches that land the machine on `Done`,
/// and driving the conclusion straight from the click blocked the click's
/// own UI-thread reply (the same shape `NatModeChoiceView`'s defer button
/// avoids). Latching here just calls the machine and returns; `vm.step`'s
/// next read — driven by `OnboardingVM`'s `@Observable` invalidation, the
/// same path every other step transition in this wizard takes — does the
/// rest.
///
/// ⚠ No renewal copy — the shared summary string promises nothing about
/// auto-renewal (`ui/nests.md` § Expiry / renewal is unbuilt); this view
/// must not add its own wording.
///
/// Shared by macOS and iOS (priority #2), mirroring `AwaitingManualDnsView`.
/// Reference impls: tui `wizard/trust_prompt.rs` (the lead app), linux
/// `views/onboarding/trust_prompt.rs`.
public struct TrustPromptView: View {
    @Bindable var vm: OnboardingVM

    public init(vm: OnboardingVM) {
        self.vm = vm
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                // Untagged on purpose — see the type doc: the page's approved ID
                // set has no `page-heading`, and an extra ID would be a ui.yaml
                // deviation needing fresh user approval.
                Text(L.onboarding.trustPrompt.title)
                    .font(.title2.bold())

                automationText(Ids.trustBoxSummary, L.onboarding.trustPrompt.summary)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)

                Button(L.onboarding.trustPrompt.grantButton) {
                    _ = vm.machine.grantDefaultTrust()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .frame(maxWidth: .infinity)
                .accessibilityIdentifier(Ids.trustBoxGrantButton)
                .automationActivate(Ids.trustBoxGrantButton) {
                    _ = vm.machine.grantDefaultTrust()
                }

                Button(L.onboarding.trustPrompt.skipButton) {
                    _ = vm.machine.skipTrustPrompt()
                }
                .buttonStyle(.bordered)
                .controlSize(.large)
                .frame(maxWidth: .infinity)
                .accessibilityIdentifier(Ids.trustBoxSkipButton)
                .automationActivate(Ids.trustBoxSkipButton) {
                    _ = vm.machine.skipTrustPrompt()
                }

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
        }
    }
}
