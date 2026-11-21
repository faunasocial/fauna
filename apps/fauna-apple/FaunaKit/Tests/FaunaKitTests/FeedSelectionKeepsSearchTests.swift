#if !FAUNA_KIDS
import Testing
@testable import FaunaKit

// A feed selection never touches the search: the committed term rides every
// read (`FeedManager::select_*` keep `search_query` — `feed.md` § Where logic
// lives → *Search filter*), so the field keeps showing it. Blanking the field
// on a selection hid its clear button while the list stayed filtered, painting
// "No matching posts." under an empty search (found by the actuation sweep's
// restore, which re-chooses Local after the search step). Nest-free: with no
// manager each selection is a no-op past the field.

@MainActor
@Suite struct FeedSelectionKeepsSearchTests {
    @Test func everySelectionKeepsTheSearchFieldsTerm() async {
        let vm = FeedVM()
        vm.searchText = "Walk"
        await vm.selectLocalFeed()
        #expect(vm.searchText == "Walk")
        await vm.selectTrendingFeed()
        #expect(vm.searchText == "Walk")
        await vm.selectFeed("feed-1")
        #expect(vm.searchText == "Walk")
        await vm.selectImported(token: "bluesky")
        #expect(vm.searchText == "Walk")
        #expect(vm.showsSearchBar)  // its clear button stays reachable
    }

    @Test func clearingTheSearchStillEmptiesTheField() {
        let vm = FeedVM()
        vm.searchText = "Walk"
        vm.clearSearch()
        #expect(vm.searchText == "")
    }
}
#endif
