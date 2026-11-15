import FaunaFFISwift
import Foundation
import Testing

@testable import FaunaKit

/// Headless pins for iOS's engine roster — the reading the launch folder pass
/// asks before the projection walk's take-over claims an adoption marker
/// (`succession-cut.md` ruling (11)(l), *one consumer per marker*). The roster
/// is assembled from injected sources, so no File Provider domain and no photo
/// binding of the real app is touched (testing.md § conventions point 10).
@Suite struct EngineRosterTests {
    private let actor = String(repeating: "aa", count: 32)

    private struct DomainsUnreadable: Error {}

    private func domainId(_ folderId: String) throws -> String {
        try #require(FileProviderDomainIdentity(actorIdHex: actor, folderId: folderId)).domainId
    }

    /// Every File Provider domain and the photo binding are named — the
    /// domains in their actor-scoped spelling, the photo set as its bare ref.
    @Test func domainsAndPhotoBindingAreAllNamed() throws {
        let d1 = try domainId("local:1")
        let d2 = try domainId("local:2")
        let roster = EngineRoster(readDomainIds: { [d1, d2] }, readPhotoBinding: { "local:9" })
        #expect(try roster.boundFolderRefs() == [d1, d2, "local:9"])
    }

    /// A device that never resolved its photo library lends its domains alone.
    @Test func noPhotoBindingNamesTheDomainsOnly() throws {
        let d1 = try domainId("local:1")
        let roster = EngineRoster(readDomainIds: { [d1] }, readPhotoBinding: { nil })
        #expect(try roster.boundFolderRefs() == [d1])
    }

    /// A domain list that cannot be read is an unread roster — it throws,
    /// never answers empty (an empty roster would let the walk claim every set).
    @Test func anUnreadableDomainListThrows() {
        let roster = EngineRoster(
            readDomainIds: { throw DomainsUnreadable() }, readPhotoBinding: { "local:9" })
        #expect(throws: FfiError.self) { try roster.boundFolderRefs() }
    }

    /// A bundle that embeds no File Provider extension hosts no File Provider
    /// engine, so it has no domain binding to read: its roster names no domain
    /// and never asks the OS — whose domain list fails in such a bundle, which
    /// would otherwise read as an unread roster and stop every take-over on it.
    @Test func aBundleWithNoFileProviderExtensionBindsNoDomain() throws {
        let ids = try EngineRoster.domainIds(
            embedsFileProvider: false, read: { throw DomainsUnreadable() })
        #expect(ids.isEmpty)
    }

    /// A bundle that embeds the extension still reads the OS's list, and a
    /// failing read still throws (the unread roster).
    @Test func aBundleWithTheExtensionReadsTheDomainList() throws {
        let d1 = try domainId("local:1")
        #expect(try EngineRoster.domainIds(embedsFileProvider: true, read: { [d1] }) == [d1])
        #expect(throws: DomainsUnreadable.self) {
            try EngineRoster.domainIds(embedsFileProvider: true, read: { throw DomainsUnreadable() })
        }
    }

    /// The extension is recognised by its extension point in the embedded
    /// plug-ins' Info dictionaries, and only by it.
    @Test func theFileProviderExtensionIsFoundByItsExtensionPoint() {
        let fp: [String: Any] = [
            "NSExtension": ["NSExtensionPointIdentifier": "com.apple.fileprovider-nonui"]
        ]
        let other: [String: Any] = [
            "NSExtension": ["NSExtensionPointIdentifier": "com.apple.widgetkit-extension"]
        ]
        #expect(EngineRoster.embedsFileProvider(pluginInfos: [other, fp]))
        #expect(!EngineRoster.embedsFileProvider(pluginInfos: [other]))
        #expect(!EngineRoster.embedsFileProvider(pluginInfos: []))
    }

    /// A registered identifier that scopes no set to an account (a bare ref, a
    /// set name, a legacy spelling) is no engine binding — the extension never
    /// serves it and the reconcile removes it — so it is skipped, never passed
    /// on: shared Rust fails the whole roster on an entry naming no set.
    @Test func aStrayUnscopedDomainIsSkipped() throws {
        let d1 = try domainId("local:1")
        let roster = EngineRoster(
            readDomainIds: { ["Photos", d1, "local:1:legacy"] }, readPhotoBinding: { nil })
        #expect(try roster.boundFolderRefs() == [d1])
    }
}
