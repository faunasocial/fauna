import FaunaFFISwift
import Testing

@testable import FaunaKit

/// Headless pins for the macOS **two-root Media-badge fold**
/// (`on-demand-files.md` § On-Demand Files → Apple File Provider binding, *state
/// unification* — macOS has two consent domains, and each set's `fsid-<ref>.db`
/// lives in its HOST's root): the routing decision and the slice replacement are
/// pure functions; the `fileStates` reads themselves are the live inch.
@MainActor
struct SyncStatesStoreTests {
    // MARK: - Routing

    /// A set whose local presence is a registered File Provider domain is hosted by
    /// the sandboxed extension, so its DB is in the container — the steward read.
    @Test func aFileProviderBoundSetReadsTheContainer() {
        #expect(
            SyncStatesStore.readRoot(folderId: "local:1", fileProviderBound: ["local:1"], hasContainer: true)
                == .container)
    }

    /// Every other set this device hosts — an agent-bound location, the app's own
    /// one-shot sets — reads the host's own root (the user domain on macOS).
    @Test func everyOtherSetReadsTheHostRoot() {
        #expect(
            SyncStatesStore.readRoot(folderId: "local:1", fileProviderBound: ["local:2"], hasContainer: true)
                == .host)
        #expect(
            SyncStatesStore.readRoot(folderId: "local:1", fileProviderBound: [], hasContainer: true)
                == .host)
    }

    /// No container root attached (iOS, an e2e launch, an unreachable container):
    /// the host's root IS the only root, FP-bound or not.
    @Test func withoutAContainerRootEvenABoundSetReadsTheHost() {
        #expect(
            SyncStatesStore.readRoot(folderId: "local:1", fileProviderBound: ["local:1"], hasContainer: false)
                == .host)
    }

    /// Registered identifiers resolve to the ACTIVE account's refs only:
    /// another account's same-numbered ref (a domain lingering across a
    /// switch) and an identifier from before actor scoping name no set this
    /// session reads, so neither can route a set to the container.
    @Test func boundRefsAreTheActiveAccountsOnly() throws {
        let a = String(repeating: "aa", count: 32)
        let b = String(repeating: "bb", count: 32)
        let ownDocs = try #require(FileProviderDomainIdentity(actorIdHex: a, folderId: "local:1"))
        let theirPics = try #require(FileProviderDomainIdentity(actorIdHex: b, folderId: "local:2"))
        #expect(
            SyncStatesStore.fileProviderBoundRefs(
                domainIds: [ownDocs.domainId, theirPics.domainId, "local:3"], actorIdHex: a)
                == ["local:1"])
    }

    // MARK: - Slice replacement

    /// A refreshed set replaces its slice wholesale — a file the engine no longer
    /// tracks loses its badge — and never touches another set's slice.
    @Test func aRefreshReplacesOnlyThatSetsSlice() {
        let before: [String: SyncDisplayState] = [
            "docs/a.txt": .localOnly,
            "docs/b.txt": .localOnly,
            "pics/x.jpg": .synced,
        ]
        let after = SyncStatesStore.replacingSlice(
            of: before, folder: "docs",
            with: [FfiSyncFileState(path: "a.txt", state: .synced, sizeBytes: 1)])

        #expect(after["docs/a.txt"] == .synced)
        #expect(after["docs/b.txt"] == nil)
        #expect(after["pics/x.jpg"] == .synced)
        #expect(after.count == 2)
    }

    /// A set the device hosts nowhere contributes nothing — and clears any stale
    /// badges it had (a host switch leaves the old root's rows behind; the fold
    /// reads the CURRENT root, which is empty).
    @Test func anEmptyReadClearsTheSetsBadges() {
        let before: [String: SyncDisplayState] = ["docs/a.txt": .localOnly]
        let after = SyncStatesStore.replacingSlice(of: before, folder: "docs", with: [])
        #expect(after.isEmpty)
    }
}
