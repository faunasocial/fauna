import Testing
@testable import FaunaKit

// Guards for the owner-side folder Sharing seam (`FolderSharedWithSection` /
// `FolderShareSheet` + the `DevicesMachineVM` sharing gestures).
//
// The share / remove / roster-read orchestration lives in shared Rust
// (`folders_share` / `folders_remove_member` / `members_list_actors`,
// unit-tested in `libs/fauna-ffi` + `libs/fauna-client-folders`) and the
// rendering / IDs are proven by the cross-app e2e (`test_folders.py`
// owner-side, run for macos/ios via the macOS e2e harness). This in-process guard
// covers the thin Swift VM seam neither reaches: the sharing gestures must be safe
// to call before `configure` (no machine / no `api` yet) — a navigation that
// renders an expanded shared row before the WS-RPC connect lands must not crash on
// an early roster-load / share / remove.
@Test @MainActor func sharingGesturesAreSafeBeforeConfigure() async {
    let vm = DevicesMachineVM()
    #expect(vm.folderActors.isEmpty)
    #expect(vm.sharingError == nil)

    // Roster read with no `api`: no-op, no crash, no entry recorded, no page error
    // (an unreadable roster never blanks the page — folders.md § Sharing).
    await vm.loadFolderActors(name: "docs")
    #expect(vm.folderActors["docs"] == nil)
    #expect(vm.sharingError == nil)

    // Remove with no `api`: bails before touching the FFI / page error.
    await vm.removeFolderMember(name: "docs", memberActorIdHex: "ab", groupIdHex: "cd")
    #expect(vm.sharingError == nil)

    // Share with no `api`: reports failure (`false`) without surfacing a glue error.
    let shared = await vm.shareFolder(name: "docs", recipientInput: "alice@fauna.social")
    #expect(shared == false)
    #expect(vm.sharingError == nil)

    // Grant edit with no `api`: bails before the write AND before the re-read.
    await vm.setFolderMemberAccess(name: "docs", memberActorIdHex: "ab", access: "writer", byteCap: nil)
    #expect(vm.folderActors["docs"] == nil)
    #expect(vm.sharingError == nil)
}

// The member-access pickers paint the shared catalog, keyed by the wire values
// the e2e `select` drives.
@Test func memberAccessPickerOffersTheSharedCatalog() {
    #expect(memberAccessOptions().map(\.value) == ["reader", "writer"])
}

// The writer warnings' inputs: a Reader grant reaches nobody outside the set, a
// Writer grant on a public folder does, and a blank cap parses as uncapped.
@Test func writerWarningInputsComeFromSharedRust() {
    #expect(writerGrantReach(access: "reader", audience: "public", paywalled: false) == nil)
    #expect(writerGrantReach(access: "writer", audience: "public", paywalled: false) != nil)
    #expect(parseCountI64(input: "") == nil)
}
