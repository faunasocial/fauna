// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// Every picture and video of one post, in body order — one `post-image` per picture element and
/// one `video-thumbnail` per video element of ``documentMediaElements(_:)`` (render-model.md
/// § D6b: paint every item, never the first), each carrying its settled `alt` (§ D6c →
/// *Proxied video*, the `alt` paragraph). The one painter the shared list card and both apps'
/// post details use, so the walk is written once; mirrors tui's `feed::media_elements`.
///
/// The surface decides only its own chrome: the list card caps each picture's height, opens the
/// `image-lightbox` on a tap and shows a blob picture's C2PA badge; the details pass none of
/// these. A picture whose source is unavailable paints nothing, as before.
public struct PostMediaList: View {
    let vm: FeedVM
    let document: RenderDocument
    let home: FfiPostBlobHome?
    var maxImageHeight: CGFloat?
    var showC2pa: Bool
    var openLightbox: ((PostImageSource) -> Void)?
    var onPlayed: ((Double, Double) -> Void)?
    var onPlaying: ((Bool) -> Void)?

    public init(vm: FeedVM, document: RenderDocument, home: FfiPostBlobHome?,
                maxImageHeight: CGFloat? = nil, showC2pa: Bool = false,
                openLightbox: ((PostImageSource) -> Void)? = nil,
                onPlayed: ((Double, Double) -> Void)? = nil,
                onPlaying: ((Bool) -> Void)? = nil) {
        self.vm = vm
        self.document = document
        self.home = home
        self.maxImageHeight = maxImageHeight
        self.showC2pa = showC2pa
        self.openLightbox = openLightbox
        self.onPlayed = onPlayed
        self.onPlaying = onPlaying
    }

    public var body: some View {
        ForEach(Array(documentMediaElements(document).enumerated()), id: \.offset) { _, element in
            switch element {
            case .image, .proxiedImage:
                picture(element)
            case .video(let hash, let alt):
                VideoThumbnailView(hash: hash, alt: alt,
                                   resolve: { await vm.videoPlayback(hash, home: home) },
                                   onPlayed: onPlayed, onPlaying: onPlaying)
            case .proxiedVideo(let path, let alt):
                // A bridged post's `ProxiedVideo` (§ D6c → Proxied video): glyph + path; a tap
                // plays it through the nest's media proxy on the shared ticket.
                VideoThumbnailView(proxiedPath: path, alt: alt,
                                   resolve: { await vm.proxiedVideoPlayback(path) },
                                   onPlayed: onPlayed, onPlaying: onPlaying)
            }
        }
    }

    @ViewBuilder
    private func picture(_ element: DocumentMediaElement) -> some View {
        // Asking for the source kicks this picture's byte load (`FeedVM.postImage(for:home:)`).
        if let source = vm.postImage(for: element, home: home), !source.isUnavailable {
            if let openLightbox {
                let open = { openLightbox(source) }
                PostImage(source: source, alt: element.alt, activate: open)
                    .frame(maxHeight: maxImageHeight)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                    .contentShape(Rectangle())
                    .onTapGesture(perform: open)
            } else {
                PostImage(source: source, alt: element.alt)
                    .frame(maxHeight: maxImageHeight)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
            }
            // Provenance is a blob's (`ui/media.md` § C2PA provenance); a proxied picture
            // carries no badge, as on tui.
            if showC2pa, case .image(let hash, _) = element {
                PostImageC2paBadge(hash: hash, check: { await vm.hasC2pa($0, home: home) })
            }
        }
    }
}
#endif // !FAUNA_KIDS
