import Testing
import Foundation
@testable import FaunaKit

// The `post-image` two-shape rule (`docs/goal/ui/media.md` § Encryption at rest).
//
// apple's image path is a URL SwiftUI fetches and decodes itself, so — like web's
// `<img src>`, and unlike the five apps that hold the bytes — it has to ask
// `FeedManager::is_sealed_media(hash)` BEFORE rendering. The whole security half
// of that is one branch: a sealed item must never resolve to the blob URL,
// because the URL serves AEAD ciphertext. These pin the branch itself, which is
// the mechanism; the eye is only needed for "does the photo appear".
//
// The shared half is covered tier_1 in `libs/fauna-feed` (`open_media_bytes`,
// `is_sealed_media_marks_only_an_unlocked_posts_own_items`,
// `unlock_gated_post_opens_its_media_items`).

private let blobURL = URL(string: "https://nest.example/api/v1/blob/abc123")!

@Test func unsealedMediaPaintsThePlainBlobUrl() {
    let source = PostImageSource.resolve(isSealed: false, blobURL: blobURL, opened: nil)
    guard case .url(let url) = source else {
        Issue.record("a public post's image is the URL itself, got \(source)")
        return
    }
    #expect(url == blobURL)
}

@Test func sealedMediaNeverResolvesToTheBlobUrl() {
    // The load-bearing one. `opened: nil` is every render before the fetch lands,
    // and the URL is right there — handing it back would `GET` ciphertext into an
    // image decoder.
    let source = PostImageSource.resolve(isSealed: true, blobURL: blobURL, opened: nil)
    guard case .sealedPending = source else {
        Issue.record("a sealed item with no opened bytes must wait, got \(source)")
        return
    }
}

@Test func sealedMediaPaintsTheOpenedBytesOnceTheyLand() {
    let opened = FaunaPlatformImage()
    let source = PostImageSource.resolve(isSealed: true, blobURL: blobURL, opened: opened)
    guard case .decoded = source else {
        Issue.record("opened bytes are what a sealed item paints, got \(source)")
        return
    }
}

@Test func noBlobUrlPaintsNothing() {
    // Pre-auth: `FeedVM.blobURL` is nil because there is no `APIClient` yet. The
    // call sites skip the whole media block, as they did before the seal branch.
    #expect(PostImageSource.resolve(isSealed: false, blobURL: nil, opened: nil).isUnavailable)
}

@Test func alreadyOpenedBytesDoNotOverrideAnUnsealedItem() {
    // A hash the manager does not call sealed keeps the URL path — including its
    // `?thumb=1` variant, which only a nest-served blob has — even if bytes for
    // it happen to be cached. `is_sealed_media` is the single decider.
    let source = PostImageSource.resolve(isSealed: false, blobURL: blobURL, opened: FaunaPlatformImage())
    guard case .url = source else {
        Issue.record("is_sealed_media decides the shape, not the cache, got \(source)")
        return
    }
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
    #expect(PostImageSource.resolve(isSealed: false, blobURL: blobURL, opened: nil).placeholderText == nil)
    #expect(PostImageSource.sealedPending.placeholderText == nil)
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
