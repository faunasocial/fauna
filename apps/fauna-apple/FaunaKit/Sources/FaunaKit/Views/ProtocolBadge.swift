import SwiftUI

/// Renders one `protocol-badge` per classified post source. The comma-separated
/// wire `source` field (`docs/goal/ui/feed.md` § Posts) is classified by the
/// shared `classifySources` UniFFI façade (`fauna-ffi`, returning
/// `FfiSourceBadge { id, label, glyph }`); apple keeps only its `SourceGlyph →
/// emoji` rendering, keyed off the precomputed `glyph` concept — the **same**
/// `ConversationsUI.glyphEmoji` map the conversations rail uses, so the badge and
/// the rail can't drift (the D5 within-client rail-vs-badge unification;
/// `docs/goal/architecture/render-model.md` § Deltas → D5). Lifted from the old
/// per-id SF-Symbol/color switch (priority #1/#2/#4 — `feed.md` § Where logic
/// lives; web `ProtocolBadge.svelte` / android `ProtocolBadge.kt` are the prior
/// art, both emoji-keyed off the same `glyph`). The canonical display label
/// (e.g. activitypub → "Fediverse") rides `accessibilityLabel`; apple's badge
/// stays icon-only visually. An empty/unknown `source` classifies to no badges
/// (matching web/android).
public struct ProtocolBadge: View {
    public let source: String

    public init(source: String) {
        self.source = source
    }

    public var body: some View {
        HStack(spacing: 3) {
            ForEach(Array(classifySources(sourceField: source).enumerated()), id: \.offset) { _, badge in
                // Emoji-in-`Text` through the shared `SourceGlyph → emoji` map —
                // identical to the conversations rail's `protocol-icon` (no SF
                // Symbol, since SF Symbols has no fox/butterfly and mixing an
                // emoji fox beside an SF envelope reads inconsistently). The
                // rail renders the same way, so badge + rail share one map.
                // `automationText` (not a bare `Text` + `.accessibilityIdentifier`)
                // registers the read for the in-process e2e driver — its sibling
                // badges in this same HStack (`UnverifiedSourceBadge`,
                // `DelegatedOriginBadge`, `GatedPostBadge`) already do; this one
                // was the lone unregistered member (the unregistered-member bug class).
                automationText(Ids.protocolBadge, ConversationsUI.glyphEmoji(badge.glyph))
                    .font(.caption2)
                    .accessibilityLabel(badge.label)
            }
        }
    }
}
