import SwiftUI

/// App-root warning surface for off-box deployment-seed custody — the apple leg
/// of the box-recovery TRACK 1 fan-out. The launch glue (`MailEnableGlue.selfHealDeploymentSeedCustody`)
/// sets `session.recoveryCustodyWarning` when custody is **not confirmed** (a BR-2
/// `refusedMismatch`, an unavailable hand-off, or a thrown leg);
/// this renders the shared `WarningBanner` — the `warning-message` cross-app
/// e2e contract — pinned at the top of each app's authed root, tap-to-dismiss.
///
/// Shared FaunaKit (macOS + iOS), the apple idiom of linux's `ActionResult::Failed`
/// toast + android's launch `showWarning` banner (priority #2/#3). Renders nothing
/// when the warning is `nil` (custodied, idempotent re-claim, an expected
/// multi-nest no-op, or no seed to custody) — and this is the **first** real
/// `WarningBanner` render in the apple apps (the `warning-message` id had been
/// declared-present-but-never-shown).
public struct RecoveryCustodyBanner: View {
    let session: SessionState

    public init(session: SessionState) {
        self.session = session
    }

    public var body: some View {
        // Reading `session.recoveryCustodyWarning` here tracks it via Observation,
        // so the launch glue setting it (off the authed-launch Task) reveals the
        // banner without any explicit binding.
        if let message = session.recoveryCustodyWarning {
            WarningBanner(message: message)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.thinMaterial)
                .contentShape(Rectangle())
                // Tap to acknowledge + dismiss (android clears its banner on the
                // next navigation; tap-to-dismiss is the apple idiom). Clearing
                // unmounts the inner WarningBanner, whose `onDisappear` resets
                // `AppMessages.warning`.
                .onTapGesture { session.recoveryCustodyWarning = nil }
        }
    }
}
