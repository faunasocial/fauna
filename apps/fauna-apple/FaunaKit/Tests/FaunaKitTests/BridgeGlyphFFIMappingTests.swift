import Testing
import Foundation
@testable import FaunaKit

// A consented bridge app's declared glyph rides `FfiBridgeStatus.glyph` into
// `BridgeInfo` and paints the Bridges card title through the shared
// `bridgeCardTitle` (`bridges.md` § Coverage contract, outcome 11); a
// first-party row carries none and keeps the bare name. The emoji map itself
// is pinned in `fauna_client_bridges::card_title`'s own tests — these guard
// only the Swift seam.

private func status(glyph: String?) -> FfiBridgeStatus {
    FfiBridgeStatus(
        id: "matrix", name: "Matrix", available: true, linked: false,
        identity: nil, mode: nil, settings: [],
        supportsFollows: false, supportsFollowRequests: false,
        linkModes: nil, error: nil, glyph: glyph)
}

@Test func bridgeInfoFromFfiKeepsDeclaredGlyph() {
    let info = BridgeInfo(ffi: status(glyph: "globe"))
    #expect(info.id == "matrix")
    #expect(info.glyph == "globe")
    #expect(info.cardTitle == bridgeCardTitle(name: "Matrix", glyph: "globe"))
    #expect(info.cardTitle != "Matrix")
    #expect(info.cardTitle.hasSuffix(" Matrix"))
}

@Test func firstPartyBridgeInfoKeepsNilGlyphAndBareName() {
    let info = BridgeInfo(ffi: status(glyph: nil))
    #expect(info.glyph == nil)
    #expect(info.cardTitle == "Matrix")
}
