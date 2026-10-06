/// Pure eviction-observation diff, shared by the macOS appex and (at M4) the iOS
/// appex. The OS evicts materialized File Provider items behind the provider's
/// back ("Remove Download" / storage pressure) and never calls the extension
/// about it, so on the refresh tick the appex queries
/// `NSFileProviderManager.enumeratorForMaterializedItems` and reconciles the
/// host's dehydration bookkeeping (`host.evict(rel)` → `mark_placeholder`)
/// against what the OS actually still holds. This is the *diff* half — pure and
/// framework-free (no `import FileProvider`), so it stays unit-testable via
/// plain `swift test` (`FaunaKitTests`); the OS query is the appex's live inch.
public enum FileProviderEviction {
    /// The rels to demote: everything the host believes is materialized
    /// (`Synced` rows — `host.hydratedRels()`) that the OS's materialized-item
    /// enumerator no longer lists.
    ///
    /// Direction is load-bearing: only *host-hydrated*
    /// rels are candidates. A post-conflict-resolve `Placeholder` row whose
    /// loser bytes still sit materialized on the OS's disk is absent from
    /// `hydratedRels` by construction, so the tick can neither evict it nor
    /// misread it as a fresh local edit — nothing on this path ingests.
    /// Extra OS-side identifiers (directories, the root container, items the
    /// host has not folded yet) are simply ignored: the diff only ever shrinks
    /// the host's hydrated set toward the OS's truth.
    public static func evictionCandidates(
        hydratedRels: [String], materializedRels: Set<String>
    ) -> [String] {
        hydratedRels.filter { !materializedRels.contains($0) }
    }
}
