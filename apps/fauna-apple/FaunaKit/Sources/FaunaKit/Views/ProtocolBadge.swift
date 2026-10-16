// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
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
/// (e.g. activitypub → "Fediverse") rides `accessibilityLabel`; a first-party
/// badge stays icon-only visually. An empty/unknown `source` classifies to no
/// badges (matching web/android).
///
/// `bridgeRoster` is the feed snapshot's `bridgeRoster`: a token naming a listed
/// bridge is that bridge's badge, which paints its declared label beside the
/// glyph (`paintsLabel`; `ui/feed.md` § Implementation status today,
/// `SourceKind::Bridged`).
public struct ProtocolBadge: View {
    public let source: String
    public let bridgeRoster: [BridgeIdentitySnapshot]

    public init(source: String, bridgeRoster: [BridgeIdentitySnapshot]) {
        self.source = source
        self.bridgeRoster = bridgeRoster
    }

    /// The badge's visible text: the glyph, with the declared label beside it
    /// on a bridged badge.
    static func text(_ badge: FfiSourceBadge) -> String {
        let glyph = ConversationsUI.glyphEmoji(badge.glyph)
        return badge.paintsLabel ? "\(glyph) \(badge.label)" : glyph
    }

    public var body: some View {
        HStack(spacing: 3) {
            ForEach(Array(classifySources(sourceField: source, bridges: bridgeRoster).enumerated()), id: \.offset) { _, badge in
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
                automationText(Ids.protocolBadge, Self.text(badge))
                    .font(.caption2)
                    .accessibilityLabel(badge.label)
            }
        }
    }
}
#endif // !FAUNA_KIDS
