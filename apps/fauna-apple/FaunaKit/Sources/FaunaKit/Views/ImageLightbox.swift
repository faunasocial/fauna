import SwiftUI

/// A wrapper to make an image source usable as a `fullScreenCover`/`.sheet` item.
///
/// Carries the ``PostImageSource`` the card resolved rather than a bare URL: a
/// tier-restricted post's attachment has no URL that renders it, so a lightbox
/// handed the blob URL would `GET` AEAD ciphertext (`ui/media.md` § Encryption
/// at rest). The card already holds the opened bytes — the tap just passes them on.
public struct LightboxItem: Identifiable {
    public let id = UUID()
    public let source: PostImageSource

    public init(source: PostImageSource) {
        self.source = source
    }
}

/// Full-screen image viewer with pinch-zoom, drag-to-pan, tap-to-dismiss
/// (ui.yaml `image-lightbox`). Shared macOS+iOS — `statusBarHidden`/
/// `fullScreenCover(item:)` are iOS-real / macOS-shimmed to `.sheet`
/// (`CrossPlatformUI.swift`), so this compiles and behaves uniformly on both.
public struct ImageLightbox: View {
    let source: PostImageSource
    let onDismiss: () -> Void

    @State private var scale: CGFloat = 1.0
    @State private var offset: CGSize = .zero
    @State private var lastOffset: CGSize = .zero

    public init(source: PostImageSource, onDismiss: @escaping () -> Void) {
        self.source = source
        self.onDismiss = onDismiss
    }

    public var body: some View {
        ZStack {
            Color.black.opacity(0.9)
                .ignoresSafeArea()
                .onTapGesture {
                    if scale <= 1.0 {
                        onDismiss()
                    }
                }

            content
        }
        .statusBarHidden()
        .accessibilityIdentifier(Ids.imageLightbox)
        // Presence-only anchor for `is_visible("image-lightbox")`.
        .automationValue(Ids.imageLightbox, text: { "" })
    }

    /// The image itself. A public post's blob is fetched and decoded by
    /// `AsyncImage`; a sealed one arrives already opened, since only the shared
    /// manager can turn its bytes into pixels (`ui/media.md` § Encryption at rest).
    @ViewBuilder private var content: some View {
        switch source {
        case .url(let url):
            AsyncImage(url: url) { phase in
                switch phase {
                case .empty:
                    ProgressView()
                        .tint(.white)
                case .success(let image):
                    zoomable(image)
                case .failure:
                    undecodable
                @unknown default:
                    EmptyView()
                }
            }
        case .decoded(let image):
            zoomable(Image(platformImage: image))
        case .sealedPending, .proxiedPending:
            ProgressView()
                .tint(.white)
        case .unavailable:
            undecodable
        }
    }

    /// The pinch-zoom / drag-to-pan / tap-to-dismiss image, shared by both sources.
    @ViewBuilder private func zoomable(_ image: Image) -> some View {
        image
            .resizable()
            .scaledToFit()
            .scaleEffect(scale)
            .offset(offset)
            .gesture(
                SimultaneousGesture(
                    MagnifyGesture()
                        .onChanged { value in
                            let newScale = value.magnification * (scale < 1.0 ? 1.0 : scale)
                            scale = min(max(newScale, 1.0), 5.0)
                        }
                        .onEnded { _ in
                            scale = min(max(scale, 1.0), 5.0)
                            if scale <= 1.0 {
                                withAnimation(.spring()) {
                                    offset = .zero
                                    lastOffset = .zero
                                }
                            }
                        },
                    DragGesture()
                        .onChanged { value in
                            guard scale > 1.0 else { return }
                            offset = CGSize(
                                width: lastOffset.width + value.translation.width,
                                height: lastOffset.height + value.translation.height
                            )
                        }
                        .onEnded { _ in
                            lastOffset = offset
                        }
                )
            )
            .onTapGesture {
                if scale <= 1.0 {
                    onDismiss()
                }
            }
    }

    private var undecodable: some View {
        Image(systemName: "photo")
            .font(.system(size: 48))
            .foregroundStyle(.white.opacity(0.6))
    }
}
