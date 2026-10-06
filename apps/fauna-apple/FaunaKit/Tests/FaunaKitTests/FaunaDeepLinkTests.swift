import FaunaDeepLink
import Foundation
import Testing

/// The `fauna://` FP context-action vocabulary (`FaunaDeepLink` +
/// `FileProviderAction`) — parse/build round-trips, the identity/peer
/// fallthrough, and the windows-parity folder rule. The Info.plist pin at the
/// bottom is what keeps the appex's declared actions and the Swift vocabulary
/// from drifting (the identifiers live in both places by necessity).
@Test func deepLinkRoundTripsShare() {
    let link = FaunaDeepLink.folderShare(set: "My Documents")
    let url = link.url
    #expect(url != nil)
    #expect(FaunaDeepLink.parse(url!) == link)
}

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
    #expect(FaunaDeepLink.parse(URL(string: "https://folder/x?action=share")!) == nil)
}

@Test func versionsWithoutAPathIsInvalid() {
    #expect(FaunaDeepLink.parse(URL(string: "fauna://folder/docs?action=versions")!) == nil)
}

@Test func unknownActionIsInvalidAndBareSetIsShare() {
    #expect(FaunaDeepLink.parse(URL(string: "fauna://folder/docs?action=exfiltrate")!) == nil)
    // A bare set link (no action) reads as the set-level share/open default.
    #expect(
        FaunaDeepLink.parse(URL(string: "fauna://folder/docs")!)
            == .folderShare(set: "docs"))
}

/// The windows `context_menu::leaf_hidden_for_folder` twin: Share stays on
/// folders; the per-file version leaf hides.
@Test func folderRuleMatchesTheWindowsLeafSet() {
    #expect(!FileProviderAction.share.hiddenForFolder)
    #expect(FileProviderAction.versions.hiddenForFolder)
}

@Test func shareIsSetLevelAndVersionsNeedsAFileRel() {
    // Share ignores the selection (root container arrives as "").
    #expect(
        FileProviderAction.share.deepLink(set: "docs", rels: [""])
            == .folderShare(set: "docs"))
    // Versions requires a real rel — a root-container selection yields nothing.
    #expect(FileProviderAction.versions.deepLink(set: "docs", rels: [""]) == nil)
    #expect(
        FileProviderAction.versions.deepLink(set: "docs", rels: ["a/b.txt"])
            == .fileVersions(set: "docs", rel: "a/b.txt"))
}

/// Every `FileProviderAction` raw value must be declared in the FP UI appex's
/// Info.plist (`NSExtensionFileProviderActions`) — the plist is the OS-facing
/// copy of the vocabulary, and this pin is what makes the duplication safe.
@Test func uiAppexPlistDeclaresEveryAction() throws {
    let plist = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()  // FaunaKitTests
        .deletingLastPathComponent()  // Tests
        .deletingLastPathComponent()  // FaunaKit
        .appendingPathComponent("../Fauna-FileProviderUI/Info.plist").standardizedFileURL
    let contents = try String(contentsOf: plist, encoding: .utf8)
    for action in FileProviderAction.allCases {
        #expect(
            contents.contains(action.rawValue),
            "\(action.rawValue) missing from Fauna-FileProviderUI/Info.plist")
    }
    // The folder rule's plist mirror: versions constrains folders away, share
    // is unconditional.
    #expect(contents.contains("TRUEPREDICATE"))
    #expect(contents.contains("public.folder"))
}
