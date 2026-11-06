import SwiftUI

/// Transparency-at-creation notice (`account-age-band.md` § App surface →
/// *Age-band surfaces*; `invite-code-supervised-notice`): a checked invite code
/// that carries a supervised designation discloses the guardian (and the age
/// band, when the code was minted with one) BEFORE redemption. The shared
/// onboarding machine derives the text (`InviteRequestSnapshot.supervisedNotice`,
/// `nil` unless the code is valid and supervised); the shells only render it,
/// over their own platform type scale (`font`). The text embeds a nested
/// `family.age_band.*` key, hence the nested renderer.
@ViewBuilder
public func guardianSupervisedNotice(_ notice: LocalizedText?, font: Font) -> some View {
    if let notice {
        automationText(Ids.inviteCodeSupervisedNotice, renderLocalizedTextNested(notice))
            .font(font)
            .fontWeight(.semibold)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}
