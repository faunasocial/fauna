import SwiftUI

/// Transparency-at-creation notice (`family-safety.md` § Wire & data shape —
/// `invite-code-supervised-notice`): a checked out-of-band invite code that
/// carries a supervised designation discloses the guardian BEFORE
/// redemption, off the additive `supervisedBy` on the peek-only
/// `fauna.account.invite_code.verify` reply. Only a *valid* code shows it —
/// idle / verifying / invalid never do. Byte-identical on macOS
/// (`MacInviteRequestView`) and iOS (`InviteRequestView`) before this
/// extraction, both hand-rolling the same condition + copy over their own
/// platform type scale (`font`).
@ViewBuilder
public func guardianSupervisedNotice(for state: OobCodeState, font: Font) -> some View {
    if case .valid(_, let guardian) = state, let guardian, !guardian.isEmpty {
        automationText(Ids.inviteCodeSupervisedNotice,
                       L.family.supervisedNoticeOnboarding(guardian: guardian))
            .font(font)
            .fontWeight(.semibold)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}
