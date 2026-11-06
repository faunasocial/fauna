import Testing
import Foundation
@testable import FaunaKit

// Inline video playback in the `video-thumbnail` slot
// (`docs/goal/architecture/render-model.md` § D6c → *Inline playback*).
//
// What plays is the shared `FeedManager::playback_source` decision; these pin the
// apple glue around it: the `state` the element publishes (`idle` / `loading` /
// `playing` / `error`), the turn of a `PlaybackSource` into something `AVPlayer`
// can open, the hardened temp file a sealed video plays from, and — over the real
// H.264 fixture — that the player the view hosts reaches `playing` with its
// position advancing. The app-level pin is the e2e
// `test_tapping_a_video_plays_it_in_place` (macos and ios).

/// `tests/fixtures/tiny-video.mp4` — the 1 s, 64x64 H.264 clip the e2e posts.
private let fixtureVideo: URL = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // FaunaKitTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // FaunaKit
    .deletingLastPathComponent()  // fauna-apple
    .deletingLastPathComponent()  // apps
    .deletingLastPathComponent()  // repo root
    .appendingPathComponent("tests/fixtures/tiny-video.mp4")

@Suite struct InlineVideoPlaybackTests {

    // MARK: the state machine

    @Test func aVideoIsIdleUntilItIsTapped() {
        // Never autoplay: nothing but the tap leaves `idle`.
        var machine = VideoPlaybackMachine()
        #expect(machine.phase == .idle)
        machine.resolved(.stream(nestUrl: "http://nest", path: "/api/v1/blob/ab"))
        machine.playerStartedPlaying()
        #expect(machine.phase == .idle)
        #expect(machine.media == nil)
    }

    @Test func aTapLoadsThenThePlayerSaysWhenItPlays() {
        var machine = VideoPlaybackMachine()
        let started = machine.tap()
        #expect(started)
        #expect(machine.phase == .loading)

        // The source resolving is not playback: `playing` is the player's word.
        let media = VideoPlaybackMedia.stream(nestUrl: "http://nest", path: "/api/v1/blob/ab")
        machine.resolved(media)
        #expect(machine.phase == .loading)
        #expect(machine.media == media)

        machine.playerStartedPlaying()
        #expect(machine.phase == .playing)
    }

    @Test func aSecondTapWhileLoadingOrPlayingIsIgnored() {
        var machine = VideoPlaybackMachine()
        machine.tap()
        let whileLoading = machine.tap()
        #expect(!whileLoading)
        machine.resolved(.stream(nestUrl: "http://nest", path: "/api/v1/blob/ab"))
        machine.playerStartedPlaying()
        let whilePlaying = machine.tap()
        #expect(!whilePlaying)
        #expect(machine.phase == .playing)
    }

    @Test func nothingPlayableIsAnError() {
        var machine = VideoPlaybackMachine()
        machine.tap()
        machine.resolved(nil)
        #expect(machine.phase == .error)
        #expect(machine.media == nil)
    }

    @Test func aPlayerFailureIsAnErrorAndATapRetries() {
        var machine = VideoPlaybackMachine()
        machine.tap()
        machine.resolved(.stream(nestUrl: "http://nest", path: "/api/v1/blob/ab"))
        machine.playerFailed()
        #expect(machine.phase == .error)

        // The retry starts from a clean slot — the failed source is dropped.
        let retried = machine.tap()
        #expect(retried)
        #expect(machine.phase == .loading)
        #expect(machine.media == nil)
    }

    @Test func thePublishedStateIsTheSharedVocabulary() {
        // `drivers/base.py::get_attr` documents these four strings for every app.
        #expect(VideoPlaybackPhase.idle.rawValue == "idle")
        #expect(VideoPlaybackPhase.loading.rawValue == "loading")
        #expect(VideoPlaybackPhase.playing.rawValue == "playing")
        #expect(VideoPlaybackPhase.error.rawValue == "error")
    }

    // MARK: the shared decision → what the player opens

    @Test func aUrlSourceStreamsFromTheNestAndASealedOneIsOpened() {
        #expect(VideoPlaybackPlan(.url(url: "/api/v1/blob/ab")) == .stream(path: "/api/v1/blob/ab"))
        #expect(VideoPlaybackPlan(.sealed(hash: "ab")) == .openSealed(hash: "ab"))
        #expect(VideoPlaybackPlan(.unplayable(reason: "not a video block")) == .unplayable)
    }

    @Test func aTicketedProxiedPathStreamsWithItsTicketWhole() throws {
        // A `ProxiedVideo`'s answer is the proxied path with the ticket the shared
        // manager minted appended (`playback_source`, § D6c answer 4) — the same
        // `.stream` branch a blob video takes, so one join serves both arms.
        let ticketed = "/api/v1/bluesky/media?url=https%3A%2F%2Fvideo.bsky.app%2Fwatch%2Fa%2Fb.mp4"
            + "&exp=1700000000&sig=AbC-_d%2B%3D"
        #expect(VideoPlaybackPlan(.url(url: ticketed)) == .stream(path: ticketed))

        // Byte-for-byte: the proxy verifies `sig` over the exact query, so nothing
        // may be re-encoded, reordered or dropped on the way to the reader, which
        // takes the path string as it is; the private-scheme asset carries it too.
        let media = VideoPlaybackMedia.stream(nestUrl: "https://nest.example:8443", path: ticketed)
        #expect(media.source == ticketed)
        let asset = try #require(NestAssetResourceLoader.assetURL(path: ticketed))
        #expect(asset.absoluteString == "fauna-nest://" + ticketed)
        let items = try #require(URLComponents(url: asset, resolvingAgainstBaseURL: false)?.queryItems)
        #expect(items.first { $0.name == "exp" }?.value == "1700000000")
        #expect(items.first { $0.name == "sig" }?.value == "AbC-_d+=")
        #expect(items.first { $0.name == "url" }?.value == "https://video.bsky.app/watch/a/b.mp4")
    }

    // MARK: the sealed leg's temp file

    @Test func anOpenedSealedVideoLandsInAnOwnerOnlyFile() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("fauna-playback-test-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let opened = try Data(contentsOf: fixtureVideo)

        let file = try SealedPlaybackFile.write(opened, in: directory)

        #expect(try Data(contentsOf: file) == opened)
        // AVFoundation picks its reader off the extension of a local file.
        #expect(file.pathExtension == "mp4")
        let fileMode = try FileManager.default.attributesOfItem(atPath: file.path)[.posixPermissions] as? NSNumber
        #expect(fileMode?.intValue == 0o600)
        let directoryMode = try FileManager.default.attributesOfItem(atPath: directory.path)[.posixPermissions] as? NSNumber
        #expect(directoryMode?.intValue == 0o700)

        SealedPlaybackFile.remove(file)
        #expect(!FileManager.default.fileExists(atPath: file.path))
    }

    @Test func theFileExtensionFollowsTheContainer() {
        func isoBmff(brand: String) -> Data {
            Data([0, 0, 0, 0x18]) + Data("ftyp".utf8) + Data(brand.utf8) + Data(count: 12)
        }
        #expect(SealedPlaybackFile.fileExtension(for: isoBmff(brand: "isom")) == "mp4")
        #expect(SealedPlaybackFile.fileExtension(for: isoBmff(brand: "qt  ")) == "mov")
        // Anything else still gets a name AVFoundation will try; a container it
        // cannot read fails in the player, whose error view is the error UI.
        #expect(SealedPlaybackFile.fileExtension(for: Data("not a video".utf8)) == "mp4")
    }

    // MARK: the player the view hosts
    //
    // No test here creates an `AVPlayer`: creating one opens a WindowServer
    // connection, and on a machine with no display adapter that handshake never
    // returns (the main thread parks in `SLSServerPort`, out of any time limit's
    // reach), which would hang the whole `swift test` run. The player itself —
    // `playing`, the position advancing — is the tier_3 test's to witness.

    @MainActor
    @Test func aSourceThatResolvesToNothingLeavesNoPlayer() async throws {
        let playback = InlineVideoPlayer()
        #expect(playback.phase == .idle)
        #expect(playback.position == 0)
        playback.activate { nil }
        let deadline = Date().addingTimeInterval(10)
        while playback.phase == .loading, Date() < deadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        #expect(playback.phase == .error)
        #expect(playback.player == nil)
    }

    // A play is attention (`engagement-cues.md`): the tap engages, and a resolve
    // that yields nothing — or a stop — disengages; each change is relayed once.
    @MainActor
    @Test func theTapEngagesAndANilResolveOrAStopDisengages() async throws {
        let playback = InlineVideoPlayer()
        var noted: [Bool] = []
        playback.onPlaying = { noted.append($0) }
        playback.activate { nil }
        #expect(noted == [true])
        let deadline = Date().addingTimeInterval(10)
        while noted.count < 2, Date() < deadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        #expect(noted == [true, false])
        playback.stop()
        #expect(noted == [true, false])

        playback.activate { try? await Task.sleep(nanoseconds: 5_000_000_000); return nil }
        #expect(noted == [true, false, true])
        playback.stop()
        #expect(noted == [true, false, true, false])
    }

    // MARK: the streamed arm's byte source
    //
    // A nest URL is handed to no OS component (`transport-trust.md` § Transport
    // trust → *The native-player leg*): the `Url` arm is an asset on a private
    // scheme whose loads `NestAssetResourceLoader` answers from the pinned reader.
    // The answers are pinned here over a fake reader; that AVFoundation plays
    // through them against a self-signed nest is the TLS twin's to witness.

    @Test func aStreamPublishesItsPathAndHeldBytesTheSharedArmSpelling() {
        #expect(VideoPlaybackMedia.stream(nestUrl: "https://127.0.0.1:3000", path: "/api/v1/blob/ab").source
                == "/api/v1/blob/ab")
        // Held bytes publish `<arm>:<hash>` (`fauna_feed::held_video_source`),
        // never the temp file's URL — a random path no test can predict.
        let file = URL(fileURLWithPath: "/tmp/fauna-playback/x.mp4")
        #expect(VideoPlaybackMedia.file(file, held: .sealed, hash: "ab12").source == "sealed:ab12")
        #expect(VideoPlaybackMedia.file(file, held: .home, hash: "ab12").source == "home:ab12")
    }

    @Test func theStreamedAssetIsOnThePrivateSchemeWithTheLoaderAsItsDelegate() throws {
        let loader = NestAssetResourceLoader(open: { FakeRangedReader(bytes: Data()) })
        let asset = try #require(loader.asset(path: "/api/v1/media/proxy?url=x&ticket=t"))
        // A scheme AVFoundation cannot dial, so every load comes to the loader.
        #expect(asset.url.scheme == "fauna-nest")
        #expect(asset.url.absoluteString == "fauna-nest:///api/v1/media/proxy?url=x&ticket=t")
        #expect(asset.resourceLoader.delegate === loader)
    }

    @Test func theInformationRequestIsAnsweredFromTheReader() throws {
        let reader = FakeRangedReader(bytes: isoBmff(brand: "isom", size: 1000), mime: "video/mp4")
        let loader = NestAssetResourceLoader(open: { reader })
        let info = try loader.contentInformation()
        #expect(info == .init(contentType: "public.mpeg-4", contentLength: 1000, isByteRangeAccessSupported: true))
    }

    @Test func anOctetStreamIsTypedByItsContainerBrand() throws {
        // The blob route answers `application/octet-stream` for an upload whose
        // stored type is not on its allowlist; the bytes still say what they are.
        let mov = FakeRangedReader(bytes: isoBmff(brand: "qt  ", size: 64), mime: "application/octet-stream")
        #expect(try NestAssetResourceLoader(open: { mov }).contentInformation().contentType == "com.apple.quicktime-movie")
        let mp4 = FakeRangedReader(bytes: isoBmff(brand: "isom", size: 64), mime: nil)
        #expect(try NestAssetResourceLoader(open: { mp4 }).contentInformation().contentType == "public.mpeg-4")
    }

    @Test func anUnknownLengthOffersNoSeeking() throws {
        let reader = FakeRangedReader(bytes: isoBmff(brand: "isom", size: 64), mime: "video/mp4", knownLength: false)
        let info = try NestAssetResourceLoader(open: { reader }).contentInformation()
        #expect(info.contentLength == nil)
        #expect(!info.isByteRangeAccessSupported)
    }

    @Test func aRangedDataRequestIsReadAtItsOffset() throws {
        let bytes = Data((0..<255).map(UInt8.init))
        let reader = FakeRangedReader(bytes: bytes)
        var got = Data()
        try NestAssetResourceLoader(open: { reader })
            .read(offset: 100, length: 20, isCancelled: { false }, deliver: { got.append($0) })
        #expect(got == bytes.subdata(in: 100..<120))
    }

    @Test func aToTheEndRequestStopsAtTheEndAndReadsOnPastAShortAnswer() throws {
        // A chunk and a half, served in answers narrower than asked: a short
        // answer is read on, only the known total ends the loop.
        let size = Int(NestAssetResourceLoader.chunk) * 3 / 2
        let bytes = Data((0..<size).map { UInt8($0 % 251) })
        let reader = FakeRangedReader(bytes: bytes, maxAnswer: 100_000)
        var got = Data()
        try NestAssetResourceLoader(open: { reader })
            .read(offset: 10, length: nil, isCancelled: { false }, deliver: { got.append($0) })
        #expect(got == bytes.subdata(in: 10..<size))
        #expect(reader.reads.allSatisfy { $0.offset < UInt64(size) })
    }

    @Test func anUnknownLengthReadsToTheEmptyAnswer() throws {
        let bytes = Data(repeating: 7, count: 300)
        let reader = FakeRangedReader(bytes: bytes, knownLength: false)
        var got = Data()
        try NestAssetResourceLoader(open: { reader })
            .read(offset: 0, length: nil, isCancelled: { false }, deliver: { got.append($0) })
        #expect(got == bytes)
    }

    @Test func aCancelledRequestStopsReading() throws {
        let size = Int(NestAssetResourceLoader.chunk) * 4
        let reader = FakeRangedReader(bytes: Data(count: size))
        var chunks = 0
        try NestAssetResourceLoader(open: { reader })
            .read(offset: 0, length: nil, isCancelled: { chunks >= 1 }, deliver: { _ in chunks += 1 })
        #expect(chunks == 1)
        #expect(reader.reads.count == 1)
    }

    @Test func theReaderOpensOnceAndAFailedOpenIsRemembered() throws {
        var opens = 0
        let loader = NestAssetResourceLoader(open: {
            opens += 1
            throw CocoaError(.fileReadUnknown)
        })
        #expect(throws: (any Error).self) { try loader.contentInformation() }
        #expect(throws: (any Error).self) {
            try loader.read(offset: 0, length: 1, isCancelled: { false }, deliver: { _ in })
        }
        #expect(opens == 1)
    }

    @Test func aReleasedLoaderReadsNothingMore() throws {
        let reader = FakeRangedReader(bytes: Data(count: 10))
        let loader = NestAssetResourceLoader(open: { reader })
        _ = try loader.contentInformation()
        loader.release()
        #expect(throws: (any Error).self) {
            try loader.read(offset: 0, length: 1, isCancelled: { false }, deliver: { _ in })
        }
    }
}

private func isoBmff(brand: String, size: Int) -> Data {
    var data = Data([0, 0, 0, 0x18]) + Data("ftyp".utf8) + Data(brand.utf8)
    data.append(Data(count: max(size - data.count, 0)))
    return data
}

/// The UniFFI reader's face over bytes in memory: `read_at` answers at most
/// `maxAnswer` bytes (a nest's 206 may be narrower than asked), shorter at EOF,
/// empty past it.
private final class FakeRangedReader: FfiRangedNestReaderProtocol, @unchecked Sendable {
    let bytes: Data
    let mime: String?
    let knownLength: Bool
    let maxAnswer: UInt64
    private(set) var reads: [(offset: UInt64, maxLen: UInt64)] = []

    init(bytes: Data, mime: String? = "video/mp4", knownLength: Bool = true, maxAnswer: UInt64 = .max) {
        self.bytes = bytes
        self.mime = mime
        self.knownLength = knownLength
        self.maxAnswer = maxAnswer
    }

    func contentType() -> String? { mime }
    func len() -> UInt64? { knownLength ? UInt64(bytes.count) : nil }
    func readAt(offset: UInt64, maxLen: UInt64) throws -> Data {
        reads.append((offset, maxLen))
        guard offset < UInt64(bytes.count) else { return Data() }
        let end = min(UInt64(bytes.count), offset + min(maxLen, maxAnswer))
        return bytes.subdata(in: Int(offset)..<Int(end))
    }
}
