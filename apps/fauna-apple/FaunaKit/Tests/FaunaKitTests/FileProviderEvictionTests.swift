import Testing

@testable import FaunaKit

#if os(macOS)
    import FileProvider

    /// The keep of a body the holder-keeps gate holds
    /// (`relay-serving.md` § Relay serving → *A holder keeps what it wrote*;
    /// `on-demand-files-apple.md` § Apple File Provider binding): on macOS
    /// such an item declares the keep-downloaded content policy, which takes it
    /// out of the OS's own eviction while it still reads as synced; every other
    /// item inherits the domain's policy and stays evictable.
    @Suite struct FileProviderKeepTests {
        @Test func aKeptItemIsKeptDownloaded() {
            #expect(
                FileProviderKeep.contentPolicy(kept: true)
                    == .downloadEagerlyAndKeepDownloaded)
        }

        @Test func anyOtherItemInheritsTheDomainsPolicy() {
            #expect(FileProviderKeep.contentPolicy(kept: false) == .inherited)
        }
    }
#endif

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
