import SwiftUI

// The feed's paging gesture: the reader reaching the end of the post list asks
// the manager for its next page — the scroll edge tui, web, linux and android
// page on (`docs/goal/ui/feed.md` § Errors & edge cases: the paginated fetch is
// `has_more` + one in flight, with no load-more element). Twin of linux's
// `feed/scroll_edge.rs` and web's `onScroll`, with their 200-unit margin.
//
// `FeedManager::load_more` is itself a no-op without `has_more` or while a
// fetch is in flight; the latch here only keeps one arrival from asking once
// per layout pass.

/// How near the bottom counts as reaching it — web's and linux's margin.
private let feedScrollEdgeMargin: CGFloat = 200

/// The end-of-list sentinel's frame (global — the viewport is read the same
/// way, and only their relative geometry matters).
struct FeedScrollEdgeSentinelKey: PreferenceKey {
    static var defaultValue: CGRect { .zero }
    static func reduce(value: inout CGRect, nextValue: () -> CGRect) {
        let next = nextValue()
        if next != .zero { value = next }
    }
}

/// The scroll view's own frame — the visible window, not the content extent.
struct FeedScrollEdgeViewportKey: PreferenceKey {
    static var defaultValue: CGRect { .zero }
    static func reduce(value: inout CGRect, nextValue: () -> CGRect) {
        let next = nextValue()
        if next != .zero { value = next }
    }
}

public extension View {
    /// Marks the end of a paging list's content: place it after the last row,
    /// inside the scroll view `feedScrollEdge` is applied to.
    func feedScrollEdgeSentinel() -> some View {
        background(
            GeometryReader { geometry in
                Color.clear.preference(key: FeedScrollEdgeSentinelKey.self,
                                       value: geometry.frame(in: .global))
            }
        )
    }

    /// Calls `loadMore` once each time the reader scrolls the list's end
    /// (its `feedScrollEdgeSentinel`) within the margin of the viewport's
    /// bottom. Apply to the `ScrollView` itself. The latch arms only once the
    /// end has been out of reach — a list shorter than its window never pages
    /// by itself, as linux's `upper > page_size` guard keeps — and re-arms
    /// when the list grows (a page arrived), so a reader resting at the end
    /// asks again on the next move.
    func feedScrollEdge(itemCount: Int, hasMore: Bool, loadMore: @escaping () -> Void) -> some View {
        modifier(FeedScrollEdgeModifier(itemCount: itemCount, hasMore: hasMore, loadMore: loadMore))
    }
}

struct FeedScrollEdgeModifier: ViewModifier {
    let itemCount: Int
    let hasMore: Bool
    let loadMore: () -> Void

    @State private var viewport: CGRect = .zero
    @State private var sentinel: CGRect = .zero
    @State private var armed = false
    @State private var everOutOfReach = false

    func body(content: Content) -> some View {
        content
            .background(
                GeometryReader { geometry in
                    Color.clear.preference(key: FeedScrollEdgeViewportKey.self,
                                           value: geometry.frame(in: .global))
                }
            )
            .onPreferenceChange(FeedScrollEdgeViewportKey.self) { rect in
                MainActor.assumeIsolated {
                    viewport = rect
                    evaluate(viewport: rect, sentinel: sentinel)
                }
            }
            .onPreferenceChange(FeedScrollEdgeSentinelKey.self) { rect in
                MainActor.assumeIsolated {
                    sentinel = rect
                    evaluate(viewport: viewport, sentinel: rect)
                }
            }
            .onChange(of: itemCount) {
                if everOutOfReach { armed = true }
                evaluate(viewport: viewport, sentinel: sentinel)
            }
    }

    private func evaluate(viewport: CGRect, sentinel: CGRect) {
        guard hasMore, viewport != .zero, sentinel != .zero else { return }
        let near = sentinel.minY <= viewport.maxY + feedScrollEdgeMargin
        if !near {
            everOutOfReach = true
            armed = true
        } else if armed {
            armed = false
            loadMore()
        }
    }
}
