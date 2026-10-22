import Testing
import Foundation
@testable import FaunaKit

// The `post-image` decision (`docs/goal/ui/media.md` § Encryption at rest; `feed.md`
// § The read model → *A post homed on another nest*).
//
// Every post image is bytes the app fetched from the post's own home through the
// shared post-blob plane and the shared manager opened (`open_media_bytes`), then
// decoded locally — there is no platform-fetched URL, so a post homed on another
// nest reaches the same path as a same-nest one and ciphertext can never reach a
// decoder by a URL. These pin the decision itself, which is the mechanism; the eye
// is only needed for "does the photo appear".
//
// The shared half is covered tier_1 in `libs/fauna-feed` (`open_media_bytes`,
// `is_sealed_media_marks_only_an_unlocked_posts_own_items`,
// `unlock_gated_post_opens_its_media_items`) and `libs/fauna-client` (`post_blob`).

@Test func aPostImageWithNoBytesYetWaitsOnItsFetch() {
    let source = PostImageSource.resolve(signedIn: true, opened: nil)
    guard case .pending = source else {
        Issue.record("an image whose bytes have not landed waits, got \(source)")
        return
    }
}

@Test func aPostImagePaintsTheOpenedBytesOnceTheyLand() {
    let source = PostImageSource.resolve(signedIn: true, opened: FaunaPlatformImage())
    guard case .decoded = source else {
        Issue.record("opened bytes are what a post image paints, got \(source)")
        return
    }
}

@Test func noSessionPaintsNothing() {
    // Pre-auth: there is no `APIClient` to fetch with. The call sites skip the
    // whole media block.
    #expect(PostImageSource.resolve(signedIn: false, opened: nil).isUnavailable)
}

// A bridged post's picture (`docs/goal/architecture/render-model.md` § D6c): a
// `ProxiedImage` block whose `path` is nest-relative. Its bytes are plaintext from
// the reader's own nest, fetched with the session bearer — so never `AsyncImage`,
// which cannot carry one — and until they land the placeholder is addressed by
// the path, the same observable tui's label is (the bridged-post e2e reads it).

private let proxiedPath = "/api/v1/bluesky/media?url=https%3A%2F%2Fcdn.bsky.app%2Fimg%2Fa%40jpeg"

@Test func aProxiedImageWaitsOnItsBearerFetchLabelledByItsPath() {
    let source = PostImageSource.resolveProxied(path: proxiedPath, signedIn: true, opened: nil)
    guard case .proxiedPending(let path) = source else {
        Issue.record("a proxied picture with no bytes yet waits on its fetch, got \(source)")
        return
    }
    #expect(path == proxiedPath)
    #expect(source.placeholderText == proxiedPath)
}

@Test func aProxiedImagePaintsItsFetchedBytes() {
    let source = PostImageSource.resolveProxied(path: proxiedPath, signedIn: true, opened: FaunaPlatformImage())
    guard case .decoded = source else {
        Issue.record("fetched bytes are what a proxied picture paints, got \(source)")
        return
    }
    #expect(source.placeholderText == nil)
}

@Test func aProxiedImagePaintsNothingBeforeLogin() {
    #expect(PostImageSource.resolveProxied(path: proxiedPath, signedIn: false, opened: nil).isUnavailable)
}

@Test func onlyAProxiedPlaceholderCarriesAnAddressLabel() {
    // A blob image's slot keeps its `painted`/`placeholder` state read alone —
    // its URL is never the label.
    #expect(PostImageSource.resolve(signedIn: true, opened: nil).placeholderText == nil)
    #expect(PostImageSource.pending.placeholderText == nil)
}

@Test func theProxiedPathIsTakenOnlyWhenThePostHasNoBlobImage() {
    // tui's `proxied_post_image` precedence: the one `post-image` slot takes a
    // blob image first, exactly as before the variant existed.
    let bridged = RenderDocument(blocks: [.proxiedImage(path: proxiedPath, alt: "")])
    #expect(documentMediaProxiedImagePath(bridged) == proxiedPath)

    let mixed = RenderDocument(blocks: [.image(hash: "b1a5e0", alt: ""),
                                        .proxiedImage(path: proxiedPath, alt: "")])
    #expect(documentMediaProxiedImagePath(mixed) == nil)
    #expect(documentMediaProxiedImagePath(RenderDocument(blocks: [])) == nil)
}

@Test func theProxiedVideoPathIsTakenOnlyWhenThePostHasNoBlobVideo() {
    // `RenderDocument::proxied_post_video`'s precedence: the one `video-thumbnail` slot takes
    // a blob video first; a proxied-only post paints the path; an image-only post has none.
    let bridged = RenderDocument(blocks: [.proxiedVideo(path: proxiedPath, alt: "")])
    #expect(documentMediaProxiedVideoPath(bridged) == proxiedPath)

    let mixed = RenderDocument(blocks: [.video(hash: "b1a5e0", alt: ""),
                                        .proxiedVideo(path: proxiedPath, alt: "")])
    #expect(documentMediaProxiedVideoPath(mixed) == nil)

    let imageOnly = RenderDocument(blocks: [.proxiedImage(path: proxiedPath, alt: "")])
    #expect(documentMediaProxiedVideoPath(imageOnly) == nil)
    #expect(documentMediaProxiedVideoPath(RenderDocument(blocks: [])) == nil)
}
