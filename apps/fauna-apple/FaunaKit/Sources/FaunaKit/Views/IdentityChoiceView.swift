import SwiftUI

/// identity_choice — the wizard's entry screen (`onboarding.md` § 1 Identity):
/// create a fresh identity, import an existing one, restore one from a
/// recovery phrase, or recover a lost box.
///
/// Shared by macOS and iOS (priority #2, mirrors `TrustPromptView`) — this
/// screen was independently implemented on each platform under two
/// differently-named types (iOS's `IdentityChoiceView`, macOS's
/// `WelcomeView`) until the 2026-08-30 dedup sweep consolidated them; the
/// naming mismatch is why the duplication survived 36 prior dedup sweeps.
///
/// ui.yaml's `identity_choice` page declares no `page-heading` (unlike most
/// sibling onboarding steps) — this view must not add one.
public struct IdentityChoiceView: View {
    @Bindable var vm: OnboardingVM

    public init(vm: OnboardingVM) {
        self.vm = vm
    }

    public var body: some View {
        #if os(iOS)
            VStack(spacing: 24) {
                Spacer()
                header
                Spacer()
                buttons
                signOutResidue
                errorBanner
                Spacer()
            }
            .padding()
        #else
            VStack(spacing: 24) {
                header
                buttons
                signOutResidue
                errorBanner
            }
        #endif
    }

    @ViewBuilder
    private var header: some View {
        Image(systemName: "key.fill")
            .font(.system(size: iconSize))
            .foregroundStyle(Color.accentColor)

        Text(L.onboarding.identityChoice.title)
            .font(.title2.bold())

        Text(L.onboarding.identityChoice.subtitle)
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.center)
            .padding(.horizontal, 32)
    }

    private var iconSize: CGFloat {
        #if os(iOS)
            56
        #else
            48
        #endif
    }

    @ViewBuilder
    private var buttons: some View {
        VStack(spacing: 12) {
            Button(L.onboarding.identityChoice.createNew) {
                vm.machine.beginCreateIdentity()
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .accessibilityIdentifier(Ids.createIdentityButton)
            .automationActivate(Ids.createIdentityButton) {
                vm.machine.beginCreateIdentity()
            }

            Button(L.onboarding.identityChoice.importExisting) {
                vm.machine.beginImportIdentity()
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
            .accessibilityIdentifier(Ids.importIdentityButton)
            .automationActivate(Ids.importIdentityButton) {
                vm.machine.beginImportIdentity()
            }

            // The phrase-only IDENTITY restore (`onboarding.md` § 1 Identity) —
            // distinct from the lost-box NEST recovery below it; linux's order.
            Button(L.onboarding.identityChoice.restoreFromRecoveryKit) {
                vm.machine.beginRecoveryEntry()
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
            .accessibilityIdentifier(Ids.restoreFromRecoveryKitButton)
            .automationActivate(Ids.restoreFromRecoveryKitButton) {
                vm.machine.beginRecoveryEntry()
            }

            // Fresh-client recovery entry (box-recovery.md § Recovery UI
            // (step 4), Task E): routes through identity_import (recovery
            // intent) so the account plane decrypts, then lands on nest_recovery via
            // handle_entry (Q2-A).
            Button(L.onboarding.identityChoice.recoverLostBox) {
                vm.machine.beginRecoverLostBox()
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
            .accessibilityIdentifier(Ids.recoverLostBoxButton)
            .automationActivate(Ids.recoverLostBoxButton) {
                vm.machine.beginRecoverLostBox()
            }
        }
    }

    /// The `sign-out-residue` view (`account-scoping.md` § Erasure follows
    /// scope → *the residue surface*): present exactly while a sign-out's
    /// residue still owes work, its line naming the Remove Again button beside
    /// it. Its own element, not `error-message` — the line is state the wizard's
    /// machine does not own, and it outlives the process (the record and the
    /// launch re-check live in `SignOutResidueSurface`).
    ///
    /// The automation readers read the view model live, so a line replaced in
    /// place (the retry's refusal beside a sibling window) reads back as the
    /// new one.
    @ViewBuilder
    private var signOutResidue: some View {
        if let residue = vm.signOutResidue {
            VStack(spacing: 8) {
                Text(residue.line)
                    .font(.callout)
                    .multilineTextAlignment(.center)
                    .accessibilityIdentifier(Ids.signOutResidueMessage)
                    .automationValue(Ids.signOutResidueMessage, text: { vm.signOutResidue?.line })

                Button(L.settings.signOutResidueRetry) {
                    Task { await vm.retrySignOutResidue() }
                }
                .buttonStyle(.bordered)
                .accessibilityIdentifier(Ids.signOutResidueRetryButton)
                .automationActivate(Ids.signOutResidueRetryButton) {
                    Task { await vm.retrySignOutResidue() }
                }
            }
            .padding(.horizontal, 32)
            // `.contain` keeps both child ids queryable under the container's
            // own; `.automationValue` registers the container in the in-process
            // registry the e2e presence read consults.
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.signOutResidue)
            .automationValue(Ids.signOutResidue, text: { vm.signOutResidue?.line })
        }
    }

    @ViewBuilder
    private var errorBanner: some View {
        if let error = vm.errorMessage {
            ErrorBanner(message: error)
        }
    }
}
