import Testing
@testable import FaunaKit

// Guards for WebDAV slice 6a on apple — the DEPLOYMENT-wide enable toggle
// (admin.md § Files; webdav-server.md § Independent enablement pt 1). Distinct from the
// per-set `folder-webdav-toggle` (slice 6b): this one is the master switch that starts
// the MDA's /webdav mount, and is harmless-on — it exposes nothing until a user flags a
// set. The policy round-trip itself lives in the shared `WebdavPolicyMachine`; the
// cross-app render + toggle round-trip is `test_admin_files.py`. These cover the thin
// Swift seams: page registration and the once-only onboarding latch.

// MARK: - The admin-files page is registered in the shell

@Test func adminFilesPageIsBuiltAndCarriesTheUiYamlNavId() {
    #expect(AdminPage.files.navId == "admin-files")
    #expect(AdminPage.built.contains(.files))
    // Round-trips through the nav-id initializer the shell routes on.
    #expect(AdminPage(navId: "admin-files") == .files)
}

// The switcher lists `built` in ui.yaml `navigation.admin_pages` order: Calendar,
// Contacts, Files — Files is the files sibling of Contacts, so it must sit immediately
// after it (Contacts itself immediately follows Calendar — AdminContactsTests asserts
// that leg).
@Test func adminFilesFollowsContactsInTheSwitcherOrder() throws {
    let built = AdminPage.built
    let contacts = try #require(built.firstIndex(of: .contacts))
    let files = try #require(built.firstIndex(of: .files))
    #expect(files == contacts + 1)
}

// MARK: - The onboarding latch fires once and only once

// The checkbox records intent; the post-onboarding glue consumes the latch exactly once
// and fires `set_webdav_enabled(true)`. Consume-once is what keeps an opt-out from being
// silently re-enabled on the next launch (the same guarantee the mail latch carries).
@Test @MainActor func pendingWebdavEnableDefaultsOffAndIsIndependentOfMailAndCaldav() {
    let session = SessionState()
    #expect(session.pendingWebdavEnable == false)
    #expect(session.pendingCaldavEnable == false)
    #expect(session.pendingFirstSetupMail == nil)

    // WebDAV gates independently: latching it must not imply mail or calendar (a
    // files-only deployment fires only the webdav glue).
    session.pendingWebdavEnable = true
    #expect(session.pendingCaldavEnable == false)
    #expect(session.pendingFirstSetupMail == nil)
}
