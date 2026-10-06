import AVFoundation
import Combine
import Foundation
import Observation

// Inline video playback in the `video-thumbnail` slot — the apple glue around the
// shared decision of what plays (`docs/goal/architecture/render-model.md` § D6c →
// *Inline playback*). `FeedManager::playback_source` says WHAT plays; everything
// here is the player's own transient state, which the ruling keeps out of the
// model: the phase the element publishes, the turn of a `PlaybackSource` into
// something `AVPlayer` opens, and the `AVPlayer` itself.

/// The `state` the `video-thumbnail` element publishes — the four strings
/// `tests/e2e-unified/drivers/base.py::get_attr` documents for every app.
public enum VideoPlaybackPhase: String, Equatable, Sendable {
    /// The play glyph; nothing was asked for. A video never leaves this by itself.
    case idle
    /// Tapped: the source is resolving, or the player is buffering.
    case loading
    /// The native player reported it is playing.
    case playing
    /// Nothing playable, or the player's own failure.
    case error
}

/// The phase walk, as a pure value so the rule can be tested without a player —
/// the twin of web's `VideoThumbnail.svelte` `playState`.
public struct VideoPlaybackMachine: Equatable, Sendable {
    public private(set) var phase: VideoPlaybackPhase = .idle
    /// What the player was handed, once the source resolved.
    public private(set) var media: URL?

    public init() {}

    /// The tap. `true` when it starts a resolve — only from `idle`, or from
    /// `error` as the retry (which drops the failed source); a tap while loading
    /// or playing belongs to the native player's own controls.
    @discardableResult
    public mutating func tap() -> Bool {
        guard phase == .idle || phase == .error else { return false }
        phase = .loading
        media = nil
        return true
    }

    /// The source resolved: `nil` is nothing playable. A URL is not playback yet —
    /// the phase stays `loading` until the player says otherwise.
    public mutating func resolved(_ url: URL?) {
        guard phase == .loading, media == nil else { return }
        guard let url else {
            phase = .error
            return
        }
        media = url
    }

    /// The player reported `playing`.
    public mutating func playerStartedPlaying() {
        guard media != nil, phase == .loading else { return }
        phase = .playing
    }

    /// The player's item failed; its own error view is the error UI.
    public mutating func playerFailed() {
        guard media != nil else { return }
        phase = .error
    }
}

/// What a `PlaybackSource` asks the app to do — the apple reading of the shared
/// decision, kept pure so the mapping is tested apart from the fetches.
public enum VideoPlaybackPlan: Equatable, Sendable {
    /// Stream this nest-relative path; the app prefixes its nest origin.
    case stream(path: String)
    /// Fetch the blob, open it through the manager, play the plaintext from a file.
    case openSealed(hash: String)
    case unplayable

    public init(_ source: PlaybackSource) {
        switch source {
        case .url(let url): self = .stream(path: url)
        case .sealed(let hash): self = .openSealed(hash: hash)
        case .unplayable: self = .unplayable
        }
    }
}

/// The hardened temp file a sealed video plays from: no URL can serve its
/// plaintext, so the opened bytes go to an owner-only file `AVPlayer` reads —
/// tui's external-handoff posture (`apps/tui.md` § External media handoff).
public enum SealedPlaybackFile {
    /// Where this process keeps them, apart from everything else in the temp dir.
    public static var directory: URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("fauna-playback", isDirectory: true)
    }

    /// Write `opened` to a fresh owner-only (0600) file in an owner-only (0700)
    /// directory. The mode is set at creation, never chmod-ed afterwards, so the
    /// plaintext is at no point readable by another user.
    public static func write(_ opened: Data, in directory: URL = SealedPlaybackFile.directory) throws -> URL {
        let files = FileManager.default
        try files.createDirectory(at: directory, withIntermediateDirectories: true,
                                  attributes: [.posixPermissions: 0o700])
        // An existing directory keeps whatever mode it had; make it ours.
        try files.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
        let file = directory.appendingPathComponent(UUID().uuidString)
            .appendingPathExtension(fileExtension(for: opened))
        guard files.createFile(atPath: file.path, contents: opened,
                               attributes: [.posixPermissions: 0o600]) else {
            throw CocoaError(.fileWriteUnknown)
        }
        return file
    }

    /// Best-effort delete; a file already gone is fine.
    public static func remove(_ file: URL) {
        try? FileManager.default.removeItem(at: file)
    }

    /// AVFoundation picks its reader for a local file off the extension, and the
    /// `Video` block deliberately carries no mime (the image-vs-video branch is
    /// made once, in the shared fold) — so read the ISO-BMFF brand: QuickTime's
    /// `qt  ` is `mov`, everything else `mp4`.
    static func fileExtension(for bytes: Data) -> String {
        let head = [UInt8](bytes.prefix(12))
        if head.count == 12, Array(head[4..<8]) == Array("ftyp".utf8),
           Array(head[8..<12]) == Array("qt  ".utf8) {
            return "mov"
        }
        return "mp4"
    }
}

/// One `video-thumbnail`'s player: the phase machine plus the `AVPlayer` it
/// drives. Owned by the view that hosts it; nothing here is shared state.
@MainActor @Observable
public final class InlineVideoPlayer {
    public private(set) var machine = VideoPlaybackMachine()
    /// The native player, present once a source resolved — the view swaps the
    /// play glyph for a `VideoPlayer` over it in the same slot.
    public private(set) var player: AVPlayer?
    @ObservationIgnored private var observers: Set<AnyCancellable> = []
    /// Bumped on every tap and stop, so a resolve that outlives its tap is dropped.
    @ObservationIgnored private var attempt = 0

    public init() {}

    public var phase: VideoPlaybackPhase { machine.phase }

    /// The player's position in seconds, `0` while there is none — read live, the
    /// headless witness that frames are decoding.
    public var position: Double {
        guard let seconds = player?.currentTime().seconds, seconds.isFinite else { return 0 }
        return seconds
    }

    /// What the player was handed, for the drivers' source read.
    public var source: String? { machine.media?.absoluteString }

    /// The tap: resolve what plays, then play it. `play()` is called here and
    /// nowhere else — a video never starts by itself.
    public func activate(resolve: @escaping () async -> URL?) {
        guard machine.tap() else { return }
        tearDown()
        attempt += 1
        let attempt = attempt
        Task { @MainActor [weak self] in
            let url = await resolve()
            guard let self, attempt == self.attempt else { return }
            self.start(url)
        }
    }

    /// Back to the play glyph, releasing the player.
    public func stop() {
        attempt += 1
        tearDown()
        machine = VideoPlaybackMachine()
    }

    private func start(_ url: URL?) {
        machine.resolved(url)
        guard let url = machine.media else { return }
        let item = AVPlayerItem(url: url)
        let player = AVPlayer(playerItem: item)
        player.publisher(for: \.timeControlStatus)
            .receive(on: DispatchQueue.main)
            .sink { [weak self] status in
                guard status == .playing else { return }
                MainActor.assumeIsolated { self?.machine.playerStartedPlaying() }
            }
            .store(in: &observers)
        item.publisher(for: \.status)
            .receive(on: DispatchQueue.main)
            .sink { [weak self] status in
                guard status == .failed else { return }
                MainActor.assumeIsolated { self?.machine.playerFailed() }
            }
            .store(in: &observers)
        self.player = player
        player.play()
    }

    private func tearDown() {
        observers.removeAll()
        player?.pause()
        player = nil
    }
}
