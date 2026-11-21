import Testing
@testable import FaunaKit

// Guards for the per-set "serve over WebDAV" opt-in (`folder-webdav-toggle`,
// every OWNER row) — webdav-server.md § Independent enablement point 2.
//
// The serve orchestration itself (content-key genesis/migration + WebdavKeysBlob
// provision on enable; content-key rotation + blob re-provision on disable) lives in
// shared Rust (`FoldersAuthor::serve_set`, unit-tested in `libs/fauna-client-folders`
// + `libs/fauna-ffi`), and the rendering / index behavior is proven by the cross-app
// e2e (`test_folder_webdav_toggle.py`, run for macos/ios via the macOS e2e harness).
// These in-process guards cover the two thin Swift seams neither reaches: the hint-text
// swap and the fail-closed capability gate. The read-state cases of the toggle's shared
// answer are `OwnTierAndMailGateTests.swift`'s.

// MARK: - The hint swap (6b-2c)

// The toggle's hint is REPLACED (not supplemented) by its blocked answer's reason —
// "set up mail first" when the custody read answered no MSEK. Serving seals the keys
// blob under the mail encryption key, so the flip cannot succeed without mail.
@Test func webdavHintExplainsServingWhenTheActorCanServe() {
    let answer = folderWebdavServePossibility(read: .resolved, holdsMsek: true)
    #expect(serveWebdavHintText(answer) == L.devices.serveWebdavHint)
}

@Test func webdavHintSaysStillLoadingWithoutAnMsek() {
    // The switch waits on the mail key alone (folders.md): an answered read with no
    // key reads "still loading", never a separate set-up-mail-first arm.
    let answer = folderWebdavServePossibility(read: .resolved, holdsMsek: false)
    #expect(serveWebdavHintText(answer) == L.common.stillLoading)
    // It must REPLACE the usual hint, never read as both.
    #expect(serveWebdavHintText(answer) != L.devices.serveWebdavHint)
}

// MARK: - The capability gate is FAIL-CLOSED

// `serve_set` flips the nest `webdav_enabled` flag BEFORE it re-provisions the blob, so
// an enable by an actor with no MSEK would COMMIT the flag and only then fail `NoMsek`
// (libs/fauna-ffi/src/folders_author.rs:213). The toggle must therefore be DISABLED,
// not merely error-on-click — which means the capability must default to `false` and
// only open up once the key-bearing face has actually answered `true`.
@Test @MainActor func canServeWebdavIsFalseUntilTheCapabilityFaceAnswers() async {
    let vm = DevicesMachineVM()
    #expect(vm.canServeWebdav == false)

    // With no `api` (pre-`configure` nav), the read is a no-op and the capability
    // STAYS closed — an unreadable capability never opens the toggle.
    await vm.loadCanServeWebdav()
    #expect(vm.canServeWebdav == false)
}

@Test @MainActor func serveWebdavIsSafeBeforeConfigure() async {
    let vm = DevicesMachineVM()

    // A flip with no `api` bails before touching the FFI: the custody read has not
    // answered, so the press is refused with the toggle's own still-loading reason —
    // never a glue error, never a silent return.
    await vm.serveFolderWebdav(name: "photos", mlsGroupIdHex: nil, enable: true)
    #expect(vm.sharingError == vm.webdavAnswer.refusal)
}

// MARK: - Mail-settings reachability is READ, never re-derived (6b-2a)

// `credential_management_reachable` = `enabled || caldav_enabled || carddav_enabled ||
// serves_webdav_set` is computed ONCE in shared Rust; every app reads the field so a
// future DAV sibling widens it in one place (mail-settings.md § Credential-management
// reachability). Apple previously re-derived a NARROWER `enabled || caldavEnabled`.
@Test @MainActor func credentialManagementReachabilityIsClosedWithoutASnapshot() {
    let vm = MailSettingsVM()
    #expect(vm.credentialManagementReachable == false)
    #expect(vm.servesWebdavSet == false)
}
