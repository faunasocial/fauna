import Testing

@testable import FaunaKit

/// Headless pins for the eviction-observation diff
/// (`FileProviderEviction.evictionCandidates`) — the pure half of the appex's
/// materialized-set reconcile; the `enumeratorForMaterializedItems` query is the
/// only live inch.
@Suite struct FileProviderEvictionTests {
    @Test func anOSDroppedItemIsEvicted() {
        let out = FileProviderEviction.evictionCandidates(
            hydratedRels: ["a.txt", "sub/b.txt"],
            materializedRels: ["a.txt"]
        )
        #expect(out == ["sub/b.txt"], "hydrated but no longer materialized → evict")
    }

    @Test func aStillMaterializedItemIsKept() {
        let out = FileProviderEviction.evictionCandidates(
            hydratedRels: ["a.txt", "sub/b.txt"],
            materializedRels: ["a.txt", "sub/b.txt", "sub"]
        )
        #expect(out.isEmpty, "everything the OS still holds stays hydrated")
    }

    /// After a conflict resolve whose winner is
    /// nest-side content, the row is a `Placeholder` while the OS still holds
    /// the loser bytes materialized. That rel is absent from `hydratedRels`, so
    /// the tick must not touch it — not evict it, and (by construction: this
    /// path never ingests) not misread it as a fresh local edit.
    @Test func aPostResolvePlaceholderWithLoserBytesOnDiskIsUntouched() {
        let out = FileProviderEviction.evictionCandidates(
            hydratedRels: ["other.txt"],
            materializedRels: ["resolved.txt", "other.txt"]
        )
        #expect(
            out.isEmpty,
            "an OS-materialized item the host holds as a Placeholder is not a candidate"
        )
    }

    @Test func osExtrasLikeDirectoriesAndUnfoldedItemsAreIgnored() {
        let out = FileProviderEviction.evictionCandidates(
            hydratedRels: ["a.txt"],
            materializedRels: ["a.txt", "sub", "not-yet-folded.txt"]
        )
        #expect(out.isEmpty)
    }

    @Test func emptyHydratedSetYieldsNothing() {
        let out = FileProviderEviction.evictionCandidates(
            hydratedRels: [],
            materializedRels: ["anything.txt"]
        )
        #expect(out.isEmpty)
    }
}
