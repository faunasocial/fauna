import SwiftUI

/// Renders the feed list-card `c2pa-badge` (`ui/media.md` § C2PA provenance).
/// `check` is the VIEWER's verdict over the post-image blob's bytes —
/// ``FeedVM/hasC2pa(_:)``: a HEAD pre-filter on the uploader's `x-c2pa`
/// assertion, then a byte-level parse — never the header alone, which any
/// modified client can forge. That is tui's `Op::FetchC2pa` and windows's
/// `PostMediaOpen`; android and linux still paint from the header. Hidden until
/// the check resolves `true` —
/// no flash-on-speculative, matching every other app's badge (an errored or
/// negative check degrades to "no badge"). `.task(id:)` re-runs the check if
/// `hash` changes under the same view identity (a rebuilt card resolving a
/// different image); what must survive a rebuild — a verdict that cost a full
/// image fetch — is remembered by ``FeedVM``, not here.
///
/// Shared between both apple apps, wired once at the one call site in
/// ``PostCardBody`` (macOS `MacPostCardView` and iOS `PostCardView` are thin
/// wrappers over it), so the check + paint logic is never duplicated per
/// platform (priority #2).
///
/// The `.task` hangs off an always-present zero-size anchor, never off the
/// conditionally-absent badge content itself — a `Group { if cond { … } }`
/// whose content starts absent (the initial `verified = false`) does not
/// reliably run a `.task` attached to it on first appearance (confirmed live:
/// hardcoding `verified = true` made the check run and self-correct every
/// time; the unconditional-anchor shape below runs the check from `verified =
/// false` too). The visible badge is a `.overlay` on the anchor so hide/show
/// stays real presence/absence for the e2e registry, matching every other
/// app's badge — only the task's own host is unconditional.
public struct PostImageC2paBadge: View {
    public let hash: String
    public let check: (String) async -> Bool

    public init(hash: String, check: @escaping (String) async -> Bool) {
        self.hash = hash
        self.check = check
    }

    @State private var verified = false

    public var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            .task(id: hash) {
                verified = await check(hash)
            }
            .overlay(alignment: .leading) {
                if verified {
                    // `automationText` renders the label AND registers its read for
                    // the in-process e2e driver — the canonical presence-readable-
                    // badge pattern (`GatedPostBadge`/`UnverifiedSourceBadge`).
                    automationText(Ids.c2paBadge, L.c2pa.badgeLabel)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .help(L.conversations.detail.badgeC2pa)
                        .accessibilityLabel(L.conversations.detail.badgeC2pa)
                }
            }
    }
}
