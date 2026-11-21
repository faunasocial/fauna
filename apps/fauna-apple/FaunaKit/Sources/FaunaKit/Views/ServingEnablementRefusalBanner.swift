import SwiftUI

/// The post-claim serving enablement's refusal, said on the page the person is
/// on (`onboarding.md` § 3b *The step completes; it is not fired once*): a step
/// the nest refused for good ends that step, and the refusal is not only
/// logged. `MailEnableGlue.applyPostClaimServingEnablement` sets
/// `session.servingEnablementRefusal` from the shared step's report; this
/// renders it through the shared `ErrorBanner` — the `error-message` cross-app
/// contract — pinned at the top of each app's authed root, above whichever page
/// is showing, tap-to-dismiss.
///
/// Shared FaunaKit (macOS + iOS), the apple idiom of tui's
/// `ServingEnablementRefused` (which writes the text into the current page's
/// errors) and the same root placement as `RecoveryCustodyBanner`. Renders
/// nothing while the refusal is `nil`.
public struct ServingEnablementRefusalBanner: View {
    let session: SessionState

    public init(session: SessionState) {
        self.session = session
    }

    public var body: some View {
        if let message = session.servingEnablementRefusal {
            ErrorBanner(message: message)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.thinMaterial)
                .contentShape(Rectangle())
                .onTapGesture { session.servingEnablementRefusal = nil }
        }
    }
}
