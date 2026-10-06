import Foundation

// Search renders the shared `SearchManager`'s merged, deduplicated,
// relevance-ordered `SearchResultRow` list (`docs/goal/ui/search.md` § State &
// data shape) — the manager already resolved the badge (via the shared
// `fauna_client_search::render::content_type_badge`) and cleaned the FTS
// snippet, so a row is display-ready with no per-app re-derivation (mirrors
// `apps/fauna-linux/src/views/search.rs`'s `build_search_result_row` /
// `apps/fauna-tui/src/search.rs`'s `result_element`).

public extension SearchResultRow {
    /// Stable row key for SwiftUI lists (`content_id` is unique within its
    /// kind class post-dedup; paired with `content_type` to be safe across
    /// classes, mirroring the retired `FfiSearchResult.rowKey`).
    var rowKey: String { "\(contentType):\(contentId)" }

    /// The already-localized badge, resolved through the apple i18n pipeline.
    var localizedBadge: String { renderLocalizedText(badge) }
}
