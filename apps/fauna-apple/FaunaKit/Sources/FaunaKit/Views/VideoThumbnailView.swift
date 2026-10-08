import AVKit
import SwiftUI

/// Renders the feed `video-thumbnail` element for a folded `RenderBlock::Video` hash
/// (render-model.md § D6b) — the video twin of `post-image`, mutually exclusive with it since
/// the shared fold branches image-vs-video once per attachment.
///
/// **The element is the player host** (render-model.md § D6c → *Inline playback*): idle it is
/// the play glyph + the hash as text (no poster frame exists to paint — `MediaItem.thumbnail`/
/// `dimensions` are `None` from every writer, deliberately); a tap asks `resolve` what plays —
/// the shared `FeedManager::playback_source` decision, turned into a URL by the view model —
/// and swaps the glyph for the system `VideoPlayer` in the same slot. Never autoplay: the tap
/// is what spends the bytes. The player's own error view is the error UI — no new element.
///
/// The one registration on the container carries the hash as the element's text (so e2e still
/// reads the resolved hash) and publishes `state` (`idle` / `loading` / `playing` / `error`),
/// `position` (the player's seconds) and `source` (what it was handed) for the drivers.
public struct VideoThumbnailView: View {
    /// The element's text: a blob video's hash, or a bridged video's nest-relative path.
    public let text: String
    private let resolve: (() async -> URL?)?
    @State private var playback = InlineVideoPlayer()

    /// - Parameter resolve: the playable URL for this video, `nil` when nothing plays. Absent →
    ///   the thumbnail stays inert (no host wired the projection).
    public init(hash: String, resolve: (() async -> URL?)? = nil) {
        self.text = hash
        self.resolve = resolve
    }

    /// A bridged post's `ProxiedVideo` (render-model.md § D6c → *Proxied video*): the glyph + the
    /// opaque nest-relative `path` as text. Never fetched or parsed, and inert — the shared
    /// `playback_source` answers `Unplayable` for it until the ticket arm lands.
    public init(proxiedPath: String) {
        self.text = proxiedPath
        self.resolve = nil
    }

    public var body: some View {
        // An unconditional container hosts the registration, so the swap below never
        // re-registers the element under a second index.
        VStack(alignment: .leading, spacing: 0) {
            if let player = playback.player {
                VideoPlayer(player: player)
                    .frame(maxWidth: .infinity)
                    .frame(height: 200)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
            } else {
                Button(action: activate) {
                    HStack(spacing: 4) {
                        if playback.phase == .loading {
                            ProgressView().controlSize(.small)
                        } else {
                            Image(systemName: "play.circle.fill")
                                .foregroundStyle(.secondary)
                        }
                        Text(text)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(resolve == nil)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.videoThumbnail)
        .automationActivate(
            Ids.videoThumbnail,
            text: { text },
            attributes: {
                ["state": playback.phase.rawValue,
                 "position": String(format: "%.3f", playback.position),
                 "source": playback.source ?? ""]
            },
            perform: activate
        )
        .onDisappear { playback.stop() }
    }

    private func activate() {
        guard let resolve else { return }
        playback.activate(resolve: resolve)
    }
}
