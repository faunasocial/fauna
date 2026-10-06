import SwiftUI

/// The search-results body — progress/empty states, the relevance-ordered
/// result list, and the load-more footer — shared by macOS and iOS
/// `SearchResultsView`. Both platforms independently reimplemented this
/// block byte-for-byte (only the navigation glue and, on iOS, the
/// type-filter picker genuinely differ), which survived earlier
/// naming-mismatch dedup sweeps because the two platform files share the
/// same filename. `Group`-bodied by design: embed directly inside the caller's
/// own outer `VStack` so its spacing/alignment (which differs slightly
/// between macOS and iOS) still governs the layout.
public struct SearchResultsListView: View {
    let vm: SearchVM
    let onTapResult: (SearchResultRow) -> Void

    public init(vm: SearchVM, onTapResult: @escaping (SearchResultRow) -> Void) {
        self.vm = vm
        self.onTapResult = onTapResult
    }

    public var body: some View {
        Group {
            if vm.isSearching && vm.results.isEmpty {
                ProgressView(L.common.searching)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if vm.results.isEmpty {
                if vm.hasSearched {
                    ContentUnavailableView(L.searchPage.noResultsShort,
                        systemImage: "magnifyingglass",
                        description: Text("\(L.searchPage.noResults) \"\(vm.query)\""))
                        .accessibilityIdentifier(Ids.searchNoResults)
                        .automationValue(Ids.searchNoResults, text: { "" })
                }
            } else {
                // Eager `ScrollView { VStack }`, NOT a lazy `List` (rule 6 —
                // apple-e2e-automation.md § Registration rules): a lazy `List`
                // only realizes on-screen (+ small buffer) rows, so
                // `search-result-item` undercounted against the full result
                // set — confirmed 2026-07-18 on both platforms.
                ScrollView {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(vm.results, id: \.rowKey) { result in
                            VStack(alignment: .leading, spacing: 4) {
                                HStack {
                                    Text(result.localizedBadge)
                                        .font(.caption.bold())
                                        .foregroundStyle(.secondary)
                                    Spacer()
                                    Text(ValueFormat.relativeTime(thenMs: result.timestamp))
                                        .font(.caption2)
                                        .foregroundStyle(.secondary)
                                }
                                Text(result.snippet)
                                    .font(.subheadline)
                                    .lineLimit(2)
                            }
                            .padding(.vertical, 4)
                            .contentShape(Rectangle())
                            .onTapGesture { onTapResult(result) }
                            // In-process automation actuation (`.onTapGesture`
                            // is a native gesture the registry can't invoke) —
                            // one Entry carrying both the activate AND the
                            // text read, mirroring `post-card`. The read is the
                            // row as painted — badge, time, snippet, the text
                            // every other app's row reads back — so a row's
                            // kind is readable, not just its snippet.
                            .accessibilityIdentifier(Ids.searchResultItem)
                            .automationActivate(Ids.searchResultItem, value: {
                                "\(result.localizedBadge) \(ValueFormat.relativeTime(thenMs: result.timestamp)) \(result.snippet)"
                            }) {
                                onTapResult(result)
                            }
                        }

                        if vm.isSearching {
                            HStack {
                                ProgressView()
                                    .controlSize(.small)
                                Text(L.common.searching)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        } else if vm.hasMore {
                            // ui.yaml § search-controls: shown only when the
                            // result count hits the page limit (a partial page
                            // means the server has no more rows); clicking
                            // bumps the limit and re-fires the search.
                            Button(L.common.loadMore) {
                                Task { await vm.loadMore() }
                            }
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 4)
                            .accessibilityIdentifier(Ids.searchLoadMoreButton)
                            .automationActivate(Ids.searchLoadMoreButton) {
                                Task { await vm.loadMore() }
                            }
                        }
                    }
                    .padding(.horizontal)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }

            if let error = vm.errorMessage {
                ErrorBanner(message: error)
                    .padding()
            }
        }
    }
}
