import SwiftUI

/// Renders the `delegated-origin-badge` on a post-card **iff** this client
/// verified the post's signed envelope AND it was signed by a **delegated
/// authoring sub-key** rather than the account's own identity key — i.e. an
/// external ATProto app wrote the post as the account
/// (`docs/goal/behavior/atproto-pds-full.md` § Problem 1 → D10 → *Audit*,
/// ratified 2026-07-29; `docs/goal/principles.md` § a capability grant is
/// audited from the user's own app).
///
/// This is what makes D10's grant *audited* rather than merely revocable: the
/// signed bytes **are** the log, read client-side, so a user scrolling their own
/// feed can tell which posts they did not personally write. Reads the single
/// shared signal `fauna_core::render::AuthoringOriginStatus` off
/// `PostSummary.authoringOrigin` (priorities #1/#2 — never re-deriving origin
/// per client), exactly as the sibling `UnverifiedSourceBadge` reads
/// `VerificationStatus`, one field over.
///
/// **The two negative arms are the security content of this badge, not an
/// optimization — do not "helpfully" widen the predicate.** `.direct` (the
/// account signed it itself) is the overwhelmingly common case, so badging it
/// would say nothing. `.unknown` deliberately covers *both* the undecoded
/// nest-index list card *and* the verification-**failed** case, and the latter
/// is the one that matters: an unverified wire's `signer_auth` cert is precisely
/// the part nothing authenticated, so reading origin off a failed envelope would
/// let a forgery paint itself as "merely delegated" — the exact inversion of an
/// audit surface. A `Failed` post therefore shows `UnverifiedSourceBadge` and
/// never this one.
///
/// The badge names the **fact**, never an app: exactly one authoring sub-key is
/// minted per account, so nothing in the signed bytes says *which* external app
/// wrote the post. Lead app tui (`feed/mod.rs`, 2026-07-31) renders the same
/// bare `feed.delegated_origin` string with no glyph; shared between both apple
/// apps (macOS `MacPostCardView`, iOS `PostCardView`) and painted
/// independently on the quoted-post embed (`QuotedPostCard`) off the folded
/// `RenderBlock::QuotedPost::authoring_origin`.
public struct DelegatedOriginBadge: View {
    public let authoringOrigin: AuthoringOriginStatus

    public init(authoringOrigin: AuthoringOriginStatus) {
        self.authoringOrigin = authoringOrigin
    }

    public var body: some View {
        if authoringOrigin == .delegated {
            // `automationText` renders the label AND registers its read for the
            // in-process e2e driver (a bare `.accessibilityIdentifier` is
            // invisible to it) — the canonical presence-readable-badge pattern,
            // the same one `UnverifiedSourceBadge` uses. The full caveat rides
            // the tooltip / accessibility label
            // (`feed.delegated_origin_tooltip`), the same split the sibling
            // badge uses.
            automationText(Ids.delegatedOriginBadge, L.feed.delegatedOrigin)
                .secondaryCaveatBadge()
                .help(L.feed.delegatedOriginTooltip)
                .accessibilityLabel(L.feed.delegatedOriginTooltip)
        }
    }
}
