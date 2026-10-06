import SwiftUI

/// The `post-image` pixels for one resolved ``PostImageSource`` — the one leaf
/// both apple apps' three post-media surfaces paint through (the shared list card
/// plus each app's own post detail), so the two-shape branch is written once.
///
/// It also owns the `post-image` identifier and its automation read, because only
/// the leaf can see whether a picture is on screen: a `.url` source's load is
/// private to `AsyncImage`'s phase. `/element/attr?attr=state` answers `painted`
/// once a decoded image has appeared and `placeholder` while a spinner stands in
/// for it — the strings linux's agent answers off its `gtk::Picture`
/// (`tests/e2e-unified/actions/feed.py::_post_image_states_in`). The caller keeps
/// the frame, the clip and the real tap gesture, and hands the tap's action in as
/// `activate`, so the driver's click and the paint read share one registration —
/// a second `post-image` registration on the same card would split them across
/// two index slots.
public struct PostImage: View {
    let source: PostImageSource
    let activate: (() -> Void)?

    /// The picture that has actually appeared. Set only by the painted view's own
    /// `.onAppear`, so nothing but a decoded image on screen can answer `painted`;
    /// read against the current `source`, so a URL that has since changed is not
    /// mistaken for the one that loaded.
    @State private var paintedKey: PaintKey?

    public init(source: PostImageSource, activate: (() -> Void)? = nil) {
        self.source = source
        self.activate = activate
    }

    public var body: some View {
        let state = paintedKey != nil && paintedKey == PaintKey(source) ? "painted" : "placeholder"
        // A proxied picture's placeholder answers `/element/text` with its path
        // (`PostImageSource.placeholderText`); every other source has no text,
        // so the read falls back to `state`, as before.
        let text = source.placeholderText
        if let activate {
            pixels
                .accessibilityIdentifier(Ids.postImage)
                .automationActivate(Ids.postImage, text: { text }, value: { state }, perform: activate)
        } else {
            pixels
                .accessibilityIdentifier(Ids.postImage)
                .automationValue(Ids.postImage, text: { text }, value: { state })
        }
    }

    @ViewBuilder
    private var pixels: some View {
        switch source {
        case .url(let url):
            AsyncImage(url: url) { phase in
                switch phase {
                case .success(let image):
                    image.resizable().scaledToFit()
                        .onAppear { paintedKey = .url(url) }
                default:
                    // Still loading, or bytes that will not decode — the same
                    // spinner for both, as the `placeholder:` form this replaces
                    // painted.
                    ProgressView()
                }
            }
        case .decoded(let image):
            Image(platformImage: image)
                .resizable()
                .scaledToFit()
                .onAppear { paintedKey = .decoded }
        case .sealedPending:
            // The bytes are being fetched and opened. The blob URL is never a
            // fallback here — it serves AEAD ciphertext.
            ProgressView()
        case .proxiedPending:
            // A bridged post's picture, its bearer fetch in flight (or failed —
            // the next render retries). The path is the element's automation text
            // only, never painted or spoken: it is an opaque nest route.
            ProgressView()
        case .unavailable:
            EmptyView()
        }
    }

    /// Which picture painted. `.decoded` carries no identity: a new opened image
    /// for the same leaf repaints the same `Image` without a fresh `.onAppear`,
    /// and it is painted all the same.
    private enum PaintKey: Equatable {
        case url(URL)
        case decoded

        init?(_ source: PostImageSource) {
            switch source {
            case .url(let url): self = .url(url)
            case .decoded: self = .decoded
            case .sealedPending, .proxiedPending, .unavailable: return nil
            }
        }
    }
}
