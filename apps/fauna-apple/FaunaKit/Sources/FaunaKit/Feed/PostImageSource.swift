import Foundation

/// What a `post-image` surface actually paints for one blob hash.
///
/// apple is one of the two apps whose post image is a URL the platform fetches
/// and decodes natively — `AsyncImage(url:)` here, `<img src>` on web — so no
/// Swift code ever holds the bytes and there is nothing to route unconditionally
/// through the shared manager. The other five apps (tui, linux, windows,
/// android) pass *every* hash through `FeedManager::open_media_bytes` before
/// decoding, with no is-this-post-restricted branch anywhere in them. That split
/// is a declared two-shape rule, not drift to resolve: `docs/goal/ui/media.md`
/// § Encryption at rest owns it and names which apps are in which shape.
///
/// So apple asks `FeedManager::is_sealed_media(hash)` *before* rendering:
///
///   - a public post's blob is plaintext on the wire, so the URL **is** the
///     render — and only that path can ever use the nest's `?thumb=1` smaller
///     blob, a *separate* blob the uploader generated (the nest cannot render
///     one; it cannot read the bytes).
///   - a tier-restricted post's attachment is AEAD-sealed under the same
///     per-post key its body opened under, so no URL can render it. The bytes
///     have to come back through `open_media_bytes` and go out as an
///     already-decoded platform image.
public enum PostImageSource {
    /// A nest blob URL the platform fetches and decodes itself.
    case url(URL)
    /// Bytes this client fetched and the shared manager opened.
    case decoded(FaunaPlatformImage)
    /// Sealed, and the bytes are not open yet — paint the placeholder while the
    /// fetch runs. Never the URL: it would serve AEAD ciphertext.
    case sealedPending
    /// A bridged post's picture (`render-model.md` § D6c, a `ProxiedImage` block)
    /// whose bytes have not landed — paint the placeholder, addressed by the
    /// nest-relative `path`. The bytes are plaintext but the route wants the
    /// session bearer, so they come back through the view model's authorized GET
    /// and go out as ``decoded(_:)`` — never a URL handed to `AsyncImage`, which
    /// cannot carry one.
    case proxiedPending(String)
    /// No blob URL at all (pre-auth) — paint nothing.
    case unavailable

    /// The decision itself, as a pure function of the three facts the view model
    /// can supply.
    ///
    /// Kept separate from ``FeedVM/postImage(_:)`` so the rule can be tested: the
    /// sealed branch is one `if` that is trivially easy to get backwards, and
    /// getting it backwards hands AEAD ciphertext to an image decoder. web keeps
    /// its half of the same rule in `$lib/media-src.ts` for the same reason.
    ///
    /// - Parameters:
    ///   - isSealed: the shared manager's own answer (`FeedManager::is_sealed_media`).
    ///     apple never guesses this — the manager holds the keys.
    ///   - blobURL: the plain `GET /api/v1/blob/<hash>` URL, `nil` before login.
    ///   - opened: the decoded bytes for this hash, if an earlier fetch landed.
    public static func resolve(isSealed: Bool,
                               blobURL: URL?,
                               opened: FaunaPlatformImage?) -> PostImageSource {
        guard isSealed else {
            // The overwhelmingly common answer, and every answer before a gated
            // post is unlocked: the plain blob URL, thumbnail variant and all.
            guard let blobURL else { return .unavailable }
            return .url(blobURL)
        }
        if let opened { return .decoded(opened) }
        return .sealedPending
    }

    /// The source for a bridged post's `ProxiedImage`, as the same pure function
    /// shape as ``resolve(isSealed:blobURL:opened:)``. No seal branch: the nest
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
