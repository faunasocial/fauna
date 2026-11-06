import FaunaDeepLink
import FaunaFFISwift
import Foundation
import Testing

@testable import FaunaKit

/// The `fauna://` FP context-action vocabulary (`FaunaDeepLink` +
/// `FileProviderAction` + the Share leaf's `FaunaShareRoute`) — the version
/// leaf's parse/build round-trip, the identity/peer fallthrough, the Share
/// leaf's FFI-free speller pinned round-trip against the shared Rust parser,
/// and the Share action's activation rule evaluated against projected items
/// (`macos.md` § Sync → *Finder's Share entry*, witness (a) and (b)). The
/// Info.plist pins are what keep the appex's declared actions and the Swift
/// vocabulary from drifting (the identifiers live in both places by necessity).
@Test func deepLinkRoundTripsVersionsWithUnicodeRel() {
    let link = FaunaDeepLink.fileVersions(set: "photos", rel: "Ferien 2026/übersicht ✓.txt")
    let url = link.url
    #expect(url != nil)
    #expect(FaunaDeepLink.parse(url!) == link)
}

@Test func identityAndPeerUrisFallThrough() {
    // Owned by the onboarding/pairing parsers — never routed as an FP action.
    #expect(FaunaDeepLink.parse(URL(string: "fauna://identity?secret=00&handle=a")!) == nil)
    #expect(FaunaDeepLink.parse(URL(string: "fauna://peer?actor_id=00")!) == nil)
    #expect(FaunaDeepLink.parse(URL(string: "https://folder/x?action=versions&path=a")!) == nil)
}

@Test func versionsWithoutAPathIsInvalid() {
    #expect(FaunaDeepLink.parse(URL(string: "fauna://folder/docs?action=versions")!) == nil)
}

@Test func theRetiredSetLevelShareFormIsNoLongerALink() {
    // `fauna://folder/<set>?action=share` (and the bare set link) retired with
    // the share-routes re-shape: Share spells the shared in-app routes now.
    #expect(FaunaDeepLink.parse(URL(string: "fauna://folder/docs?action=share")!) == nil)
    #expect(FaunaDeepLink.parse(URL(string: "fauna://folder/docs")!) == nil)
    #expect(FaunaDeepLink.parse(URL(string: "fauna://folder/docs?action=exfiltrate")!) == nil)
}

/// The windows `context_menu::leaf_hidden_for_folder` twin: Share stays on a
/// folder (the set's root); the per-file version leaf hides.
@Test func folderRuleMatchesTheWindowsLeafSet() {
    #expect(!FileProviderAction.share.hiddenForFolder)
    #expect(FileProviderAction.versions.hiddenForFolder)
}

private let localDomain = "local%3A42@" + String(repeating: "ab", count: 32)
private let foreignDomain = "foreign%3A" + String(repeating: "cd", count: 32) + "@"
    + String(repeating: "ab", count: 32)

@Test func theDomainIdentifierNamesOnlyALocalSet() {
    #expect(FaunaShareRoute.localFolderId(domainIdentifier: localDomain) == 42)
    #expect(FaunaShareRoute.localFolderId(domainIdentifier: foreignDomain) == nil)
    #expect(FaunaShareRoute.localFolderId(domainIdentifier: "Photos") == nil)
    #expect(FaunaShareRoute.localFolderId(domainIdentifier: "local%3Ax@ab") == nil)
}

@Test func shareSpellsOneRoutePerSelectedItem() {
    #expect(
        FaunaShareRoute.forItem(domainIdentifier: localDomain, rel: "")
            == .folderShare(folderId: 42))
    #expect(
        FaunaShareRoute.forItem(domainIdentifier: localDomain, rel: "a/b.txt")
            == .shareLink(folderId: 42, path: "a/b.txt"))
    // A cross-nest set is never a target; nor is a multi-item selection.
    #expect(FaunaShareRoute.forItem(domainIdentifier: foreignDomain, rel: "a.txt") == nil)
    #expect(FileProviderAction.share.url(domainIdentifier: localDomain, rels: ["a", "b"]) == nil)
    #expect(FileProviderAction.share.url(domainIdentifier: foreignDomain, rels: [""]) == nil)
    // Versions keeps its own grammar and needs a real rel.
    #expect(FileProviderAction.versions.url(domainIdentifier: localDomain, rels: [""]) == nil)
    #expect(
        FileProviderAction.versions.url(domainIdentifier: localDomain, rels: ["a/b.txt"])
            == FaunaDeepLink.fileVersions(set: localDomain, rel: "a/b.txt").url)
}

/// The Swift speller is a second speller of `fauna_core::app_route`: every
/// item shape it builds must parse, through the shared Rust parser, to the
/// route it meant — and spell exactly what the Rust builder spells.
@Test func theShareSpellerRoundTripsThroughTheRustParser() throws {
    let paths = [
        "a.txt", "Trips/2026/beach day.jpg", "Ferien/übersicht ✓.txt", "q&a=1#2.md",
        "100%/x+y.txt", "deep/er/still/file-name_~.v2",
    ]
    for path in paths {
        let url = try #require(FaunaShareRoute.shareLink(folderId: 9, path: path).url)
        #expect(parseAppRoute(uri: url.absoluteString) == .shareLink(folderId: 9, path: path))
        #expect(url.absoluteString == appRouteUri(route: .shareLink(folderId: 9, path: path)))
    }
    let root = try #require(FaunaShareRoute.folderShare(folderId: 9).url)
    #expect(parseAppRoute(uri: root.absoluteString) == .folderShare(folderId: 9))
    #expect(root.absoluteString == appRouteUri(route: .folderShare(folderId: 9)))
}

/// `Fauna-FileProviderUI/Info.plist` — the OS-facing copy of the vocabulary.
private func uiAppexPlist() throws -> [String: Any] {
    let url = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()  // FaunaKitTests
        .deletingLastPathComponent()  // Tests
        .deletingLastPathComponent()  // FaunaKit
        .appendingPathComponent("../Fauna-FileProviderUI/Info.plist").standardizedFileURL
    let data = try Data(contentsOf: url)
    return try #require(
        PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any])
}

private func activationRule(_ action: FileProviderAction) throws -> String {
    let ext = try #require(try uiAppexPlist()["NSExtension"] as? [String: Any])
    let actions = try #require(ext["NSExtensionFileProviderActions"] as? [[String: Any]])
    let entry = try #require(
        actions.first { $0["NSExtensionFileProviderActionIdentifier"] as? String == action.rawValue })
    return try #require(entry["NSExtensionFileProviderActionActivationRule"] as? String)
}

/// Every `FileProviderAction` raw value must be declared in the FP UI appex's
/// Info.plist (`NSExtensionFileProviderActions`) — the plist is the OS-facing
/// copy of the vocabulary, and this pin is what makes the duplication safe.
@Test func uiAppexPlistDeclaresEveryAction() throws {
    for action in FileProviderAction.allCases {
        _ = try activationRule(action)
    }
    // The folder rule's plist mirror: versions constrains folders away.
    #expect(try activationRule(.versions).contains("public.folder"))
}

/// An item as the activation predicate sees it: its `userInfo`, projected by
/// the same `FileProviderShareEntry` the FP appex's items use.
private func item(_ entry: String?, folder: Bool = false) -> NSDictionary {
    var dict: [String: Any] = [
        "contentType": ["identifier": folder ? "public.folder" : "public.data"]
    ]
    if let info = FileProviderShareEntry.userInfo(entry) { dict["userInfo"] = info }
    return dict as NSDictionary
}

private func offersShare(_ items: [NSDictionary]) throws -> Bool {
    let predicate = NSPredicate(format: try activationRule(.share))
    return predicate.evaluate(with: ["fileproviderItems": items] as NSDictionary)
}

/// Outcome 29 and outcome 30's second half, headless: Share is offered on an
/// eligible file and on the set's root, and nowhere else.
@Test func theShareActivationRuleOffersOnlyWhereShareHasMeaning() throws {
    let eligibleFile = item(FileProviderShareEntry.forFile(shareLinkEligible: true))
    let sealedFile = item(FileProviderShareEntry.forFile(shareLinkEligible: false))
    let subFolder = item(nil, folder: true)
    let root = item(FileProviderShareEntry.forRoot(domainIdentifier: localDomain), folder: true)
    let foreignRoot = item(
        FileProviderShareEntry.forRoot(domainIdentifier: foreignDomain), folder: true)

    #expect(try offersShare([eligibleFile]))
    #expect(try offersShare([root]))
    #expect(try !offersShare([sealedFile]), "a non-public set's file carries no link")
    #expect(try !offersShare([subFolder]), "a sub-folder is not a shareable unit")
    #expect(try !offersShare([foreignRoot]), "a cross-nest set is never a target")
    #expect(try !offersShare([eligibleFile, eligibleFile]), "a share names one item")
    #expect(try !offersShare([]))
}
