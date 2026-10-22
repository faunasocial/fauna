import Foundation
import Testing
@testable import FaunaKit

// Pin the Nests trust facet's scope line (`nests.md` § Trust facet — grants): a
// paywall grant names its folder through the shared `grant_scope_labels` door,
// and a scopeless `Revoke` line (History carries no scope) renders the folder
// alone. tui's `scope_line` is the reference shape.

private let premium = TrustFolder.named(name: "premium")
private let folderRead = TrustScope(class: "content.read", kind: "folder", tier: nil)

@Test func namedPaywallGrantShowsItsFolder() {
    #expect(scopeLine([folderRead], folder: premium) == "Your folder \"premium\"")
}

@Test func scopelessRevokeWithAFolderShowsTheFolderAlone() {
    #expect(scopeLine([], folder: premium) == "Your folder \"premium\"")
}

@Test func deletedFolderShowsTheDeletedLabel() {
    #expect(scopeLine([], folder: .deleted) == "A folder you have since deleted")
}

@Test func grantWithoutAFolderKeepsThePerTupleLabel() {
    let mail = TrustScope(class: "content.read", kind: "mail", tier: nil)
    let expected = renderLocalizedText(FaunaFFISwift.scopeLabel(s: mail))
    #expect(scopeLine([mail], folder: nil) == expected)
    #expect(scopeLine([], folder: nil) == "")
}
