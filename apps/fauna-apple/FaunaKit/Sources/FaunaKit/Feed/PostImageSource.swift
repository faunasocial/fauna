import Foundation

/// What a `post-image` surface actually paints for one blob hash.
///
/// Every post image is bytes this app fetched from the post's own home through
/// the shared post-blob plane (`FfiPostBlobHosts`), opened through
/// `FeedManager::open_media_bytes` (a sealed item opens under its post's key; any
/// other hash comes back untouched) and decoded here — the shape tui, linux,
/// windows and android keep, and the only one that reaches a post homed on another
/// nest (`docs/goal/ui/media.md` § Encryption at rest; `feed.md` § The read model
/// → *A post homed on another nest*). No platform `AsyncImage(url:)` ever dials a
/// blob route, so there is no URL case.
public enum PostImageSource {
    /// Bytes this app fetched through the plane and the shared manager opened.
    case decoded(FaunaPlatformImage)
    /// The bytes are not open yet — paint the placeholder while the fetch runs.
    /// Never ciphertext: a sealed item is decoded only after the manager opens it.
    case pending
    /// A bridged post's picture (`render-model.md` § D6c, a `ProxiedImage` block)
    /// whose bytes have not landed — paint the placeholder, addressed by the
    /// nest-relative `path`. The bytes are plaintext but the route wants the
    /// session bearer, so they come back through the view model's authorized GET
    /// and go out as ``decoded(_:)`` — never a URL handed to `AsyncImage`, which
    /// cannot carry one.
    case proxiedPending(String)
    /// Not signed in — paint nothing.
    case unavailable

    /// The decision itself, as a pure function of the two facts the view model
    /// can supply.
    ///
    /// - Parameters:
    ///   - signedIn: whether there is an `APIClient` to fetch with.
    ///   - opened: the decoded bytes for this hash, if an earlier fetch landed.
    public static func resolve(signedIn: Bool, opened: FaunaPlatformImage?) -> PostImageSource {
        if let opened { return .decoded(opened) }
        guard signedIn else { return .unavailable }
        return .pending
    }

    /// The source for a bridged post's `ProxiedImage`, as the same pure function
    /// shape as ``resolve(signedIn:opened:)``. No seal branch: the nest
    /// proxies a third party's public bytes, so there is nothing to open.
    ///
    /// - Parameters:
    ///   - path: the block's nest-relative path (`/api/v1/bluesky/media?url=…`).
    ///   - signedIn: whether there is an `APIClient` to fetch it with.
    ///   - opened: the decoded bytes for this path, if an earlier fetch landed.
    public static func resolveProxied(path: String,
                                      signedIn: Bool,
                                      opened: FaunaPlatformImage?) -> PostImageSource {
        if let opened { return .decoded(opened) }
        guard signedIn else { return .unavailable }
        return .proxiedPending(path)
    }

    /// The `post-image` element's readable text while its placeholder stands in:
    /// a proxied picture's path, the label tui paints until the bytes arrive (the
    /// bridged-post e2e reads it — `render-model.md` § D6c). `nil` for every other
    /// source, whose read stays the `painted`/`placeholder` state alone.
    public var placeholderText: String? {
        if case .proxiedPending(let path) = self { return path }
        return nil
    }

    /// Nothing to paint at all — the call sites skip the whole media block,
    /// exactly as they used to skip it on a `nil` blob URL.
    public var isUnavailable: Bool {
        if case .unavailable = self { return true }
        return false
    }
}
