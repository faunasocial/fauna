import SwiftUI

/// The `launch_identity_changed` surface — the nest's pinned deployment identity
/// changed, or a pinned nest can no longer prove any identity (the withdrawn /
/// downgrade case). `docs/goal/architecture/security.md` § Transport trust: the SSH `known_hosts` model, ratified 2026-07-13 as the **uniform
/// all-app** shape.
///
/// **Blocking, and deliberately retry-less.** Auto-entry is refused and the minted
/// bearer is dropped, so there is no `launch-retry-button` here: a retry cannot
/// change the verdict, and re-running the challenge must never silently re-pin
/// (`onboarding.md` § App-launch routing, the identity-changed row). The two ways
/// out are both explicit — "trust this nest" (forget the pin, re-TOFU, re-challenge,
/// via the machine's `trustNestIdentity()`) and "use a different nest" (the wizard
/// fallthrough at `handle_entry`).
///
/// Shared by macOS (`MacAppState.LaunchGate.identityChanged`) and iOS
/// (`AppState.identityChanged`) per priority #2 — the same split
/// `LaunchNeedsUpdateView` already uses, because iOS enters optimistically and has no
/// launch-gate enum to extend. Mirrors android `LaunchIdentityChangedScreen.kt`,
/// linux `views/launch.rs`, and web `routes/onboarding/+page.svelte` (same three
/// elements, same no-retry rule).
///
/// `pinnedHex`/`seenHex` are the fingerprints the machine's
/// `LaunchPhase::IdentityChanged` carries. They are **not** rendered as the warning
/// text (the warning is the fixed, localized `identity_changed_warning` string that
/// every app shows); they are available for a detail line, and are held here so
/// the surface can grow one without re-plumbing the phase.
public struct LaunchIdentityChangedView: View {
    private let pinnedHex: String
    private let seenHex: String?
    private let onTrust: () -> Void
    private let onUseDifferentNest: () -> Void

    public init(
        pinnedHex: String,
        seenHex: String?,
        onTrust: @escaping () -> Void,
        onUseDifferentNest: @escaping () -> Void
    ) {
        self.pinnedHex = pinnedHex
        self.seenHex = seenHex
        self.onTrust = onTrust
        self.onUseDifferentNest = onUseDifferentNest
    }

    public var body: some View {
        VStack(spacing: 16) {
            Text(L.onboarding.launch.identityChangedWarning)
                .multilineTextAlignment(.center)
                .accessibilityIdentifier(Ids.nestIdentityChangedWarning)
                .automationValue(
                    Ids.nestIdentityChangedWarning,
                    text: { L.onboarding.launch.identityChangedWarning }
                )

            Button(L.onboarding.launch.identityChangedTrust) { onTrust() }
                .accessibilityIdentifier(Ids.nestIdentityChangedTrustButton)
                .automationActivate(Ids.nestIdentityChangedTrustButton) { onTrust() }

            Button(L.launch.useDifferentNest) { onUseDifferentNest() }
                .accessibilityIdentifier(Ids.launchFallthroughButton)
                .automationActivate(Ids.launchFallthroughButton) { onUseDifferentNest() }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }
}
