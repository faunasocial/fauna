import Testing
import Foundation
@testable import FaunaKit

// Legal-takedown tombstone render (`docs/goal/behavior/moderation.md` § Categories &
// enforcement item 1). A post/message taken down under a legal obligation has its
// envelope WITHHELD by the nest — shared Rust projects a tombstone carrying only the
// reference (`RenderBlock::QuotedPost.legal_takedown_ref` / `MessageSnapshot
// .legal_takedown_ref`), with an empty body and no decrypt. The apps must paint the
// shared tombstone rather than a blank/broken embed.
//
// The projection itself is covered by `fauna-feed`/`fauna-conversations` Rust tests.
// These guard the two Swift seams the apple lift adds:
//   1. `documentQuotedPost` — surfaces the block's `legalTakedownRef` associated value
//      (previously dropped as `_`), which is what `QuotedPostCard` branches on.
//   2. `legalTakedownTombstone` + `renderLocalizedText` — the shared LocalizedText face
//      resolves to real prose carrying the reference (not an unresolved i18n key).
// The `QuotedPostCard` / `DmMessageBubble` branches themselves are view concerns,
// render-confirmed by the cross-app e2e (entrusted to the macOS e2e harness).

private let reference = "EU-DSA-2024/12345"

@Test func documentQuotedPostSurfacesLegalTakedownRef() {
    let takenDown = RenderDocument(blocks: [
        .quotedPost(postId: "abc123", author: "", body: "", verification: .unchecked,
                    authoringOrigin: .unknown, legalTakedownRef: reference, notFound: false)
    ])

    let quote = documentQuotedPost(takenDown)
    #expect(quote?.legalTakedownRef == reference)
    // The nest withholds the envelope, so there is no author or body to paint —
    // the tombstone stands in for the whole card.
    #expect(quote?.body == "")
}

@Test func documentQuotedPostLeavesOrdinaryQuoteUntombstoned() {
    let ordinary = RenderDocument(blocks: [
        .quotedPost(postId: "abc123", author: "deadbeef", body: "hello", verification: .verified,
                    authoringOrigin: .unknown, legalTakedownRef: nil, notFound: false)
    ])

    let quote = documentQuotedPost(ordinary)
    // nil ⇒ the card paints the normal author row + body, not the tombstone.
    #expect(quote?.legalTakedownRef == nil)
    #expect(quote?.body == "hello")
}

@Test func legalTakedownTombstoneResolvesToProseCarryingTheReference() {
    let rendered = renderLocalizedText(legalTakedownTombstone(reference: reference))

    // The reference is interpolated into the shared string, and the LocalizedText
    // actually resolved (an unresolved key would render as the bare i18n key).
    #expect(rendered.contains(reference))
    #expect(!rendered.isEmpty)
    #expect(!rendered.hasPrefix("moderation."))
}
