import Testing
import Foundation
@testable import FaunaKit

// The four embed-projection extractors in `DocumentBodyView.swift` delegate to the shared
// UniFFI faces (`render-model.md` § Implementation status — the four-twins-four-chances-to-
// miss-an-arm consolidation, apple's leg 2026-08-02) rather than re-walking the block tree in
// Swift. The shared faces **recurse** into block quotes / list items / task items; every
// hand-rolled twin they replaced scanned only the top level.
//
// So the property worth pinning on this side of the FFI is exactly that: an embed nested inside
// a block quote is found. Each test below would FAIL against the deleted top-level-only twin and
// passes through the delegation — which is what stops a future session from quietly re-rolling a
// `for block in document.blocks` loop that looks equivalent and silently drops nested embeds.
//
// MUTATION-PROVEN 2026-08-02: restoring the old `for block in document.blocks` scan in
// `documentMediaImageHash` fails exactly `documentMediaImageHashFindsANestedImage` and leaves the
// other 320 FaunaKit tests green — i.e. the whole existing suite is blind to this regression, which
// is the reason these exist (an assertion nobody mutation-checked is just more green).
//
// Behaviour on today's *production* documents is unchanged: the feed manager pushes
// `QuotedPost`/`Image` at top level and `inject_link_previews` only considers top-level
// paragraphs, so these are forward guards, not bug repros. The walks themselves are covered by
// `fauna-core`'s own recursion tests; these guard the Swift seam.

/// Wrap `blocks` one level deep, so a top-level-only scan finds nothing.
private func quoted(_ blocks: [RenderBlock]) -> RenderDocument {
    RenderDocument(blocks: [.blockQuote(blocks: blocks)])
}

@Test func documentMediaImageHashFindsANestedImage() {
    let hash = "b1a5e0"
    #expect(documentMediaImageHash(quoted([.image(hash: hash, alt: "")])) == hash)
    // And still nil when there is genuinely no media to fold.
    #expect(documentMediaImageHash(RenderDocument(blocks: [])) == nil)
}

@Test func documentQuotedPostFindsANestedQuote() {
    let doc = quoted([
        .quotedPost(postId: "abc123", author: "deadbeef", body: "hello", verification: .verified,
                    authoringOrigin: .unknown, legalTakedownRef: nil, notFound: false)
    ])

    let quote = documentQuotedPost(doc)
    #expect(quote?.postId == "abc123")
    #expect(quote?.body == "hello")
    // nil ⇒ the fire-once `resolveQuotedPost` guard at the call sites keeps firing.
    #expect(documentQuotedPost(RenderDocument(blocks: [])) == nil)
}

@Test func documentResolvedLinkPreviewsFindsANestedPreviewAndDropsUnresolvedOnes() {
    let doc = quoted([
        .linkPreview(url: "https://example.com/a",
                     state: .resolved(title: "A", description: "first",
                                      imageHash: "img-a", revealed: true)),
        .linkPreview(url: "https://example.com/b", state: .resolving),
        .linkPreview(url: "https://example.com/c", state: .failed),
    ])

    // Only the Resolved one yields a card — a Resolving/Failed block paints none (the inline
    // body link already shows), which the extractor now filters instead of each call site.
    let previews = documentResolvedLinkPreviews(doc)
    #expect(previews.count == 1)
    #expect(previews.first?.url == "https://example.com/a")
    #expect(previews.first?.title == "A")
    #expect(previews.first?.description == "first")
    #expect(previews.first?.imageHash == "img-a")
    #expect(previews.first?.revealed == true)
}

@Test func resolvingLinkPreviewUrlsFindsANestedResolvingPreview() {
    let doc = quoted([
        .linkPreview(url: "https://example.com/a",
                     state: .resolved(title: "A", description: "", imageHash: nil, revealed: false)),
        .linkPreview(url: "https://example.com/b", state: .resolving),
    ])

    // Fire-once by construction: a resolved block no longer yields its url, so the `.task(id:)`
    // keyed on this list settles instead of re-firing.
    #expect(resolvingLinkPreviewUrls(doc) == ["https://example.com/b"])
}

@Test func hasBlockedRemoteImagesSeesANestedUnrevealedImage() {
    // The fifth member of the same family, already delegating since apple  — pinned here
    // beside its siblings so the whole gate set has one nesting guard.
    #expect(hasBlockedRemoteImages(quoted([
        .remoteImage(url: "https://example.com/x.png", alt: "", revealed: false)
    ])))
    #expect(!hasBlockedRemoteImages(quoted([
        .remoteImage(url: "https://example.com/x.png", alt: "", revealed: true)
    ])))
}
