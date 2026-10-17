#if !FAUNA_KIDS
import Testing
import Foundation
@testable import FaunaKit

// `ui/feed.md` § Implementation status today, `SourceKind::Bridged`: a token
// naming a bridge on the feed snapshot's roster is that bridge's badge — its
// declared glyph with its label beside it — through the shared
// `classifySources` façade; with no roster entry the same token is the unknown
// source, glyph alone. Mirrors tui's and linux's
// `a_bridged_posts_badge_names_its_bridge_from_the_roster`.

@Test func aBridgedPostsBadgeNamesItsBridgeFromTheRoster() {
    let roster = [BridgeIdentitySnapshot(id: "matrix", label: "Matrix", glyph: .globe)]
    #expect(
        classifySources(sourceField: "fauna, matrix", bridges: roster).map(ProtocolBadge.text)
            == ["🦊", "🌐 Matrix"]
    )
    #expect(
        classifySources(sourceField: "fauna, matrix", bridges: []).map(ProtocolBadge.text)
            == ["🦊", "📡"]
    )
}
#endif // !FAUNA_KIDS
