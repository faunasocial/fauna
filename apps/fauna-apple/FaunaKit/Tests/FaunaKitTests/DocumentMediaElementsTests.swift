import Testing
@testable import FaunaKit

// Every media block a post carries is painted, in body order, each with its label
// (render-model.md § D6b *Accessors*; § D6c → *Proxied video*, the `alt` paragraph).
// The FaunaKit leg of tui's `every_media_block_paints_in_body_order_with_its_alt`;
// the paint itself is witnessed tier_3 by `test_feed_media_items.py`.

@Test func everyMediaBlockBecomesOneElementInBodyOrderWithItsAlt() {
    let document = RenderDocument(blocks: [
        .image(hash: "a1", alt: "A red square at dusk"),
        .video(hash: "v1", alt: "One second of a grey frame"),
        .image(hash: "a2", alt: "The same square, at noon"),
        .image(hash: "a3", alt: ""),
        .proxiedImage(path: "/media/proxy/p1", alt: ""),
        .proxiedVideo(path: "/media/proxy/p2", alt: "Bridged clip"),
    ])
    #expect(documentMediaElements(document) == [
        .image(hash: "a1", alt: "A red square at dusk"),
        .video(hash: "v1", alt: "One second of a grey frame"),
        .image(hash: "a2", alt: "The same square, at noon"),
        .image(hash: "a3", alt: L.feed.post.imageAltFallback),
        .proxiedImage(path: "/media/proxy/p1", alt: L.feed.post.imageAltFallback),
        .proxiedVideo(path: "/media/proxy/p2", alt: "Bridged clip"),
    ])
}

@Test func anUndescribedVideoTakesTheVideoFallbackNotThePictureOne() {
    let document = RenderDocument(blocks: [.video(hash: "v1", alt: "")])
    #expect(documentMediaElements(document) == [.video(hash: "v1", alt: L.feed.post.videoAltFallback)])
    #expect(L.feed.post.videoAltFallback != L.feed.post.imageAltFallback)
}

@Test func aDocumentWithoutMediaPaintsNoElement() {
    #expect(documentMediaElements(RenderDocument(blocks: [])).isEmpty)
}
