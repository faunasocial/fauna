import FaunaFFISwift
import SwiftUI

/// The terms-and-content-guidelines link: label `settings.terms_link`, opens the
/// shared `fauna_core::TERMS_URL` (read through `faunaFfiTermsUrl()`, never a
/// literal) in the browser. Used by the onboarding notice below
/// (`onboarding-terms-link`) and by Settings' About block
/// (`settings-terms-link`), macOS and iOS alike
/// (`docs/goal/behavior/terms-and-content-guidelines.md` § Element IDs).
public struct TermsLinkButton: View {
    @Environment(\.openURL) private var openURL
    private let id: String

    public init(id: String) { self.id = id }

    public var body: some View {
        Button(L.settings.termsLink) { open() }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(id)
            .automationActivate(id, text: { L.settings.termsLink }) { open() }
    }

    private func open() {
        if let url = URL(string: faunaFfiTermsUrl()) { openURL(url) }
    }
}

/// The by-continuing notice — one sentence with the link, placed above a wizard
/// page's bottom row on `invite_request`, `claim_code` and `nest_provisioning`
/// (always) and `handle_entry` (while the shared `terms_notice_visible` is
/// true). Nothing is persisted: no checkbox, no record (§ The decisions, item 3).
public struct TermsNotice: View {
    public init() {}

    public var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            automationText(Ids.onboardingTermsNotice, L.onboarding.terms.notice)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            TermsLinkButton(id: Ids.onboardingTermsLink)
                .font(.footnote)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
