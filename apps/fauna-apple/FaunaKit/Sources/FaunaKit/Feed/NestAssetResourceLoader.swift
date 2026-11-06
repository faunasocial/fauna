import AVFoundation
import Foundation
import UniformTypeIdentifiers

// The native-player leg on apple (`docs/goal/architecture/transport-trust.md`
// § Transport trust → *The native-player leg*; `render-model.md` § D6c →
// *Inline playback*, answer (1)): a nest URL is handed to no OS component.
// `AVPlayer` handed `https://<nest>/…` dials through the OS media stack, which
// validates against the system trust store alone and refuses the nest's
// self-signed floor. So a streamed video is an `AVURLAsset` on a private scheme
// AVFoundation cannot dial itself; every load it wants comes to this delegate,
// which answers from the shared, SPKI-pinned `FfiRangedNestReader`.

/// What a tapped `video-thumbnail` plays: a nest-relative path read through the
/// pinned reader, or a local file (a sealed item, or a post homed on another
/// nest, opened through the plane).
public enum VideoPlaybackMedia: Equatable, Sendable {
    /// `path` (its query carried verbatim — the playback ticket) on `nestUrl`, the
    /// exact string the WS login minted on, so the reader's pin-store key matches.
    case stream(nestUrl: String, path: String)
    /// Bytes the app holds, played from `URL`; `held` is the arm the resolve
    /// carried and `hash` the blob, which together are what `source` spells.
    case file(URL, held: HeldVideo, hash: String)

    /// What the element publishes as `source`: the nest-relative path for a
    /// stream (as every app publishes it), the shared `<arm>:<hash>` spelling
    /// for held bytes (`render-model.md` § D6c → *Inline playback*).
    public var source: String {
        switch self {
        case .stream(_, let path): return path
        case .file(_, let held, let hash): return heldVideoSource(held: held, hash: hash)
        }
    }
}

/// The `AVAssetResourceLoaderDelegate` over one `FfiRangedNestReader`. One loader
/// per `AVURLAsset`; it owns the reader, opened on the first load (off the main
/// thread — the reader blocks) and released with the player.
public final class NestAssetResourceLoader: NSObject, AVAssetResourceLoaderDelegate, @unchecked Sendable {
    /// The private scheme: nothing in AVFoundation dials it, so every load is ours.
    public static let scheme = "fauna-nest"
    /// The most one `read_at` asks for, so a to-the-end request answers in pieces
    /// the player can start on, and a cancel is seen between them.
    static let chunk: UInt64 = 256 * 1024

    private let open: () throws -> FfiRangedNestReaderProtocol
    private let lock = NSLock()
    private var reader: FfiRangedNestReaderProtocol?
    private var openError: Error?
    private var released = false
    /// AVFoundation's callbacks; serial, so they arrive in order.
    private let delegateQueue = DispatchQueue(label: "social.fauna.nest-asset-loader", qos: .utility)
    /// The blocking reads, off the delegate queue so a cancel is never stuck
    /// behind one (the reader takes concurrent reads).
    private let readQueue = DispatchQueue(label: "social.fauna.nest-asset-loader.read",
                                          qos: .utility, attributes: .concurrent)

    /// - Parameter open: opens the reader — `FfiRangedNestReader.open` in the app,
    ///   a fake in tests. Called at most once, on a read queue.
    public init(open: @escaping () throws -> FfiRangedNestReaderProtocol) {
        self.open = open
    }

    /// The loader for a stream of `path` on `nestUrl` over the real reader.
    public convenience init(nestUrl: String, path: String) {
        self.init(open: { try FfiRangedNestReader.open(nestUrl: nestUrl, path: path) })
    }

    /// `fauna-nest://` + the nest-relative path. Only the scheme matters to
    /// AVFoundation; the loader already knows what it reads.
    public static func assetURL(path: String) -> URL? {
        URL(string: scheme + "://" + path)
    }

    /// The asset the player opens: on the private scheme, every load routed here.
    /// The resource loader keeps its delegate weakly — the caller keeps `self`.
    public func asset(path: String) -> AVURLAsset? {
        guard let url = Self.assetURL(path: path) else { return nil }
        let asset = AVURLAsset(url: url)
        asset.resourceLoader.setDelegate(self, queue: delegateQueue)
        return asset
    }

    /// Drop the reader. A read in flight finishes on its own reference; every
    /// later one fails.
    public func release() {
        lock.lock()
        released = true
        reader = nil
        lock.unlock()
    }

    // MARK: AVAssetResourceLoaderDelegate

    public func resourceLoader(_ resourceLoader: AVAssetResourceLoader,
                               shouldWaitForLoadingOfRequestedResource loadingRequest: AVAssetResourceLoadingRequest) -> Bool {
        guard loadingRequest.request.url?.scheme == Self.scheme else { return false }
        readQueue.async { [self] in serve(loadingRequest) }
        return true
    }

    public func resourceLoader(_ resourceLoader: AVAssetResourceLoader,
                               didCancel loadingRequest: AVAssetResourceLoadingRequest) {
        // Nothing to undo: the read loop checks `isCancelled` between chunks and
        // stops answering; a `read_at` already in flight cannot be interrupted.
    }

    private func serve(_ loadingRequest: AVAssetResourceLoadingRequest) {
        do {
            if let info = loadingRequest.contentInformationRequest {
                let answer = try contentInformation()
                info.contentType = answer.contentType
                if let length = answer.contentLength { info.contentLength = length }
                info.isByteRangeAccessSupported = answer.isByteRangeAccessSupported
            }
            if let data = loadingRequest.dataRequest {
                try read(offset: data.requestedOffset,
                         length: data.requestsAllDataToEndOfResource ? nil : data.requestedLength,
                         isCancelled: { loadingRequest.isCancelled },
                         deliver: { data.respond(with: $0) })
            }
            if !loadingRequest.isCancelled { loadingRequest.finishLoading() }
        } catch {
            if !loadingRequest.isCancelled { loadingRequest.finishLoading(with: error) }
        }
    }

    // MARK: The answers, apart from AVFoundation's request objects

    /// What `contentInformationRequest` is told.
    struct ContentInformation: Equatable {
        var contentType: String
        var contentLength: Int64?
        var isByteRangeAccessSupported: Bool
    }

    struct ReaderUnavailable: Error {}

    func contentInformation() throws -> ContentInformation {
        let reader = try currentReader()
        let length = reader.len().map(Int64.init(clamping:))
        return ContentInformation(contentType: try Self.contentType(reader),
                                  contentLength: length,
                                  // Seeking needs the total; with none, the player reads on.
                                  isByteRangeAccessSupported: length != nil)
    }

    /// `length` bytes at `offset` (`nil` = to the end), handed to `deliver` in
    /// chunks. A short answer is read on, not taken as the end — only an empty one
    /// is EOF; a known total stops the loop without asking past it.
    func read(offset: Int64, length: Int?, isCancelled: () -> Bool,
              deliver: (Data) -> Void) throws {
        let reader = try currentReader()
        var position = UInt64(max(offset, 0))
        var end = length.map { position + UInt64(max($0, 0)) } ?? UInt64.max
        if let total = reader.len() { end = min(end, total) }
        while position < end, !isCancelled() {
            let bytes = try reader.readAt(offset: position, maxLen: min(Self.chunk, end - position))
            if bytes.isEmpty { break }
            deliver(bytes)
            position += UInt64(bytes.count)
        }
    }

    private func currentReader() throws -> FfiRangedNestReaderProtocol {
        lock.lock()
        defer { lock.unlock() }
        if released { throw ReaderUnavailable() }
        if let reader { return reader }
        if let openError { throw openError }
        do {
            let opened = try open()
            reader = opened
            return opened
        } catch {
            openError = error
            throw error
        }
    }

    /// The UTI AVFoundation picks its demuxer by. The nest's `Content-Type` when
    /// it names a video or audio type; otherwise the ISO-BMFF brand of the first
    /// bytes — the blob route answers `application/octet-stream` for an upload
    /// whose stored type is not on its allowlist (windows' rule; the sealed file's
    /// extension rule, `SealedPlaybackFile.fileExtension`).
    static func contentType(_ reader: FfiRangedNestReaderProtocol) throws -> String {
        if let mime = reader.contentType()?.split(separator: ";").first?
            .trimmingCharacters(in: .whitespaces).lowercased(),
           mime.hasPrefix("video/") || mime.hasPrefix("audio/"),
           let type = UTType(mimeType: mime) {
            return type.identifier
        }
        let head = try reader.readAt(offset: 0, maxLen: 12)
        return SealedPlaybackFile.fileExtension(for: head) == "mov"
            ? UTType.quickTimeMovie.identifier
            : UTType.mpeg4Movie.identifier
    }
}
