import SwiftUI

/// Renders the muted `unverified-source-badge` on a post-card **iff** THIS
/// client's signature verification of the post's signed envelope FAILED
/// (`docs/goal/architecture/security.md` § App display of unverified content;
/// review findings F-CL2/F-CL3). Reads the single shared signal,
/// `fauna_core::render::VerificationStatus`, off `PostSummary.verification`
/// (priorities #1/#2 — every app reads it from one place instead of
/// re-deriving validity) and shows the badge **iff** `.failed`; it is absent for
/// `.unchecked` (the default — a trusted nest-index projection with no envelope to
/// verify, the normal feed-list card) and `.verified`. The post still renders in
/// full alongside the badge — a transient key-rotation-lag false-negative must not
/// make a legitimate post silently vanish.
///
/// Mirrors the sibling `ProtocolBadge` post-card chrome; web
/// `UnverifiedSourceBadge.svelte` / linux `build_unverified_badge` are the prior
/// art, all off the same `verification == Failed` predicate. Shared between both
/// apple apps (macOS `MacPostCardView`, iOS `PostCardView`).
public struct UnverifiedSourceBadge: View {
    public let verification: VerificationStatus

    public init(verification: VerificationStatus) {
        self.verification = verification
    }

    public var body: some View {
        if verification == .failed {
            // `automationText` renders the label AND registers its read for the
            // in-process e2e driver (a bare `.accessibilityIdentifier` is invisible
            // to it) — the canonical presence-readable-badge pattern. The visible
            // text is "⚠ Unverified", matching web `UnverifiedSourceBadge.svelte`
            // and linux `build_unverified_badge` (both `⚠ {feed.unverified_source}`)
            // — priorities #1/#3. The full caveat rides the tooltip / accessibility
            // label (`feed.unverified_source_tooltip`), the same split web (`title`)
            // and linux (`set_tooltip_text`) use.
            automationText(Ids.unverifiedSourceBadge, "\u{26A0} \(L.feed.unverifiedSource)")
                .secondaryCaveatBadge()
                .help(L.feed.unverifiedSourceTooltip)
                .accessibilityLabel(L.feed.unverifiedSourceTooltip)
        }
    }
}
