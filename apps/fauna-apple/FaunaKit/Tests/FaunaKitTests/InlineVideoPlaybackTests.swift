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
        machine.resolved(URL(string: "http://nest/api/v1/blob/ab"))
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
        let url = URL(string: "http://nest/api/v1/blob/ab")!
        machine.resolved(url)
        #expect(machine.phase == .loading)
        #expect(machine.media == url)

        machine.playerStartedPlaying()
        #expect(machine.phase == .playing)
    }

    @Test func aSecondTapWhileLoadingOrPlayingIsIgnored() {
        var machine = VideoPlaybackMachine()
        machine.tap()
        let whileLoading = machine.tap()
        #expect(!whileLoading)
        machine.resolved(URL(string: "http://nest/api/v1/blob/ab"))
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
        machine.resolved(URL(string: "http://nest/api/v1/blob/ab"))
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
}
