import SwiftUI

/// Shared **sign-out** affordance for the Settings Account area, surfaced by
/// both Apple targets from one FaunaKit view — mounted in `AccountSettingsView`
/// on both macOS and iOS (uniform placement since 2026-08-25; iOS used to
/// duplicate it onto the Settings root list instead).
///
/// Renders the uniform inline destructive-confirm shape windows leads
/// (`docs/goal/ui/settings.md` § User actions): the `sign-out-button` reveals an
/// inline `sign-out-confirm-button` (mirroring `admin-factory-reset-button` →
/// `admin-factory-reset-confirm-button` and the in-app `MailSettingsView`
/// disable-confirm), so the confirm carries a drivable id with no native dialog.
/// Element IDs match `tests/e2e-unified/ui.yaml` settings (`sign-out-button`,
/// `sign-out-confirm-button`) exactly.
///
/// The shared credential wipe runs through `StatusVM.signOut`; the platform
/// re-root to onboarding (`identity_choice`) and any platform side-effect (e.g.
/// notifying a paired Apple Watch on iOS) is the caller's `onSignedOut` closure.
///
/// **`GroupBox`, not `Section`** (moved 2026-08-25 alongside the mount fix): the
/// parent `AccountSettingsView` renders in an eager `ScrollView { VStack }`, not
/// a `Form` — a `Section` there would sit below the fold with no ancestor `List`/
/// `Form` to give it chrome, and on iOS a `Form`'s `Section` is exactly the lazy
/// shape rule 6 exists to avoid (`apple-e2e-automation.md` § Registration rules
/// rule 6).
public struct SignOutSection: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?

    private let session: SessionState
    private let onSignedOut: () -> Void
    /// This window's raw per-account instance lock, as `FaunaAccounts.acquireInstanceLock`
    /// hands it to the macOS app root — passed as `signOutBlocked`'s `ownLock` so the probe
    /// can put it (and its store-root declaration) down around itself and not meet its own
    /// reflection (`erase_guard.rs`'s module doc; `account-scoping.md` § Concurrent
    /// instances). `nil` on iOS, which takes no such lock and so answers a clean "not
    /// blocked" — a raw-lock seat that omits this refuses **every** sign-out, even alone.
    private let ownLock: FfiAccountInstanceLock?
    @State private var statusVM = StatusVM()
    @State private var confirming = false
    /// The sign-out-blocked refusal line, painted on this section's own control — never
    /// onboarding's residue surface, since a refused sign-out leaves the user right here
    /// on Settings → Account, still signed in (`account-scoping.md:998`).
    @State private var error: String?

    public init(
        session: SessionState, ownLock: FfiAccountInstanceLock? = nil,
        onSignedOut: @escaping () -> Void
    ) {
        self.session = session
        self.ownLock = ownLock
        self.onSignedOut = onSignedOut
    }

    public var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Button(L.settings.signOut, role: .destructive) {
                    confirming = true
                }
                .accessibilityIdentifier(Ids.signOutButton)
                .automationActivate(Ids.signOutButton) { confirming = true }

                if confirming {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(L.settings.signOutConfirm)
                            .font(.callout)
                        HStack {
                            Button(L.settings.signOut, role: .destructive) {
                                confirmSignOut()
                            }
                            .accessibilityIdentifier(Ids.signOutConfirmButton)
                            .automationActivate(Ids.signOutConfirmButton) { confirmSignOut() }
                            Button(L.common.cancel, role: .cancel) {
                                confirming = false
                            }
                        }
                    }
                }

                if let error {
                    ErrorBanner(message: error)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// Wipe the stored credentials + local state and re-root onboarding. Shared
    /// by the confirm `Button` and its `.automationActivate` so the two can't
    /// diverge (convention: apple-e2e-automation.md § registration ergonomics).
    ///
    /// **Asks `signOutBlocked` first, before any of the gesture's side effects**
    /// (`account-scoping.md` § Concurrent instances → *An erase refuses while a
    /// sibling serves the account*) — not only in front of the credential wipe.
    /// The push-row drop below and `StatusVM.signOut`'s own first two calls
    /// (stopping the account runtime, releasing the conversations engine) are
    /// already un-enrolling this device and letting go of stores that back
    /// "still signed in"; a probe placed later would refuse only after that
    /// had already happened. On a refusal: nothing above runs, the confirm
    /// closes, and the line paints on this section's own control — matching
    /// the tui/linux reference shape's "the refusal runs on confirm, before
    /// anything destructive, and the sign-out row stays on screen".
    private func confirmSignOut() {
        error = nil
        if let blocked = signOutBlocked(
            registry: FaunaAccounts.registry(),
            baseDir: AccountStateDir.base.path,
            storeContainerDir: AccountStateDir.storeContainerDir,
            ownLock: ownLock
        ) {
            confirming = false
            // Shares Account's `error-message` with a pending persist-failure
            // message, which it must not replace (`StolenCeremonyHold`).
            let line = renderLocalizedText(blocked.line)
            if StolenCeremonyHold.shared.admits(line) { error = line }
            return
        }
        Task { @MainActor in
            if let client {
                statusVM.configure(api: client.api)
                // The leave-gesture push drop (`common.md` § Registration,
                // ruled 2026-08-30): the active actor's row falls while its
                // authority is still in hand, before the credential erase
                // below. Best-effort inside `dropActorRow()`, and nothing is
                // issued on an install with no push history.
                let deviceId = client.deviceId
                await PushManager(api: client.api, deviceId: deviceId).dropActorRow()
            }
            // `signOut` is async only because it must stop the account runtime
            // BEFORE erasing its store (see its doc) — the gesture itself is
            // unchanged, and `onSignedOut()` still runs after the wipe.
            await statusVM.signOut(sessionState: session)
            onSignedOut()
        }
    }
}
