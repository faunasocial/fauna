import Testing
import Foundation
@testable import FaunaKit

// The `c2pa-badge` verdict composition (`docs/goal/ui/media.md` § Encryption at
// rest → *C2PA provenance* — the badge-correction rule).
//
// The `x-c2pa` header is the uploader's own assertion: the nest stores
// `has_c2pa` for a public-post blob without ever reading the bytes, so a modified
// client can make it `true` over an image with no manifest. apple therefore
// treats the header as a pre-filter only (`FeedVM.hasC2pa`), and these pin what
// happens after it says `true` — the verdict comes from the bytes, routed through
// the same `open_media_bytes` seam the image itself uses, exactly as tui's
// `Op::FetchC2pa` does.
//
// The shared half is covered tier_1 in `libs/fauna-media`
// (`detect_c2pa_in_bytes`), and the shipped-artifact witness requires
// `c2pa-detect` in every shipped cdylib; the app-level pin is the e2e
// `test_a_forged_c2pa_assertion_paints_no_provenance_badge` (macos and ios).

/// A 1×1 PNG carrying no C2PA manifest: what an uploader asserting
/// `has_c2pa = true` over an image with no provenance actually posts.
private let plainPNG = Data(base64Encoded:
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")!

@Test func aManifestlessImageIsNotVerifiedHoweverLoudlyItsUploaderAsserted() {
    // The real FFI verdict, over real bytes, with `open` as the identity a
    // public post's blob gets. A verdict that trusted the header — or returned
    // `true` unconditionally — reddens this; the header never reaches this
    // function at all.
    #expect(C2paBadgeVerdict.over(plainPNG, open: { $0 }) == false)
}

@Test func theVerdictIsTheDetectorsAnswerOverTheOPENEDBytes() {
    // `open` runs first and the detector sees ITS output, never the bytes as
    // served: for a restricted post's attachment those are AEAD ciphertext, which
    // no manifest parser can read. Same seam `postImage` uses for the pixels.
    let served = Data("as the nest served it".utf8)
    let opened = Data("as the manager opened it".utf8)
    var handedToOpen: Data?
    var handedToDetect: Data?

    let verdict = C2paBadgeVerdict.over(
        served,
        open: { handedToOpen = $0; return opened },
        detect: { handedToDetect = $0; return true })

    #expect(verdict == true)
    #expect(handedToOpen == served)
    #expect(handedToDetect == opened)
}

@Test func aNegativeDetectorAnswerIsAVerdictNotAnAbsence() {
    // `false` is a completed verdict — the caller remembers it; `nil` below is
    // not, and must be told apart from it.
    let verdict = C2paBadgeVerdict.over(Data([1, 2, 3]), open: { $0 }, detect: { _ in false })
    #expect(verdict == false)
}

@Test func bytesTheManagerWillNotOpenHaveNoVerdictAtAll() {
    // A sealed item whose keys have not arrived yet: `open_media_bytes` answers
    // `nil`. That is "cannot tell yet", not "no provenance" — so no verdict, the
    // detector is never handed ciphertext, and the caller does not remember it
    // (a later render retries, the `sealedMediaImages` contract).
    var detectorCalled = false
    let verdict = C2paBadgeVerdict.over(
        Data("sealed".utf8),
        open: { _ in nil },
        detect: { _ in detectorCalled = true; return true })

    #expect(verdict == nil)
    #expect(!detectorCalled)
}
