import Foundation
import Testing
@testable import FaunaKit

// The media detail's `file-version-author` text with no connected-apps machine
// (`folder-deposit.md` § The depositor signs, the owner's device verifies, ruling
// 3): an ordinary version names its recorder alone, a version a connected app
// deposited paints the generic "a connected app via <owner>" wording — never a
// per-app writer-key match. With a machine, the shared
// `ConnectedAppsMachine::version_author` composes the line (its own Rust tests).

private func version(depositedBy: String?) -> FileVersionSummary {
    FileVersionSummary(
        versionNum: 1, manifestHash: "ab", sizeBytes: 3, createdAt: 0,
        contentKeyVersion: nil, authorDisplay: "alice", pruned: false,
        purgeAfter: nil, folderId: nil, thumbnailHash: nil, depositedBy: depositedBy)
}

@MainActor
@Test func orderlyVersionPaintsTheRecorderAlone() {
    let vm = MediaMachineVM()
    #expect(vm.versionAuthorText(version(depositedBy: nil))
            == L.media.versionAuthor(author: "alice"))
}

@MainActor
@Test func depositedVersionWithoutAMachinePaintsTheGenericWording() {
    let vm = MediaMachineVM()
    let text = vm.versionAuthorText(version(depositedBy: String(repeating: "ab", count: 32)))
    #expect(text == L.media.versionAuthorDepositedUnknown(author: "alice"))
}
