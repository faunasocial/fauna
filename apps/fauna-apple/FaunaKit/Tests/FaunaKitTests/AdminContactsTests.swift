import Testing
@testable import FaunaKit

// Guards for the CardDAV client gap closed on apple 2026-07-12 — the DEPLOYMENT-wide
// enable toggle (admin.md § Contacts; carddav-server.md § Independent enablement).
// Distinct from any future per-actor address-book surface: this is the master switch
// that starts the MDA's /carddav mount. The policy round-trip itself lives in the shared
// `CarddavPolicyMachine`; the cross-app render + toggle round-trip is
// `test_admin_contacts.py`. These cover the thin Swift seams: page registration and the
// once-only onboarding latch.

// MARK: - The admin-contacts page is registered in the shell

@Test func adminContactsPageIsBuiltAndCarriesTheUiYamlNavId() {
    #expect(AdminPage.contacts.navId == "admin-contacts")
    #expect(AdminPage.built.contains(.contacts))
    // Round-trips through the nav-id initializer the shell routes on.
    #expect(AdminPage(navId: "admin-contacts") == .contacts)
}

// The switcher lists `built` in ui.yaml `navigation.admin_pages` order, and Contacts is
// the contacts sibling of Calendar — so it must sit immediately after it.
@Test func adminContactsFollowsCalendarInTheSwitcherOrder() throws {
    let built = AdminPage.built
    let calendar = try #require(built.firstIndex(of: .calendar))
    let contacts = try #require(built.firstIndex(of: .contacts))
    #expect(contacts == calendar + 1)
}

// MARK: - The onboarding latch fires once and only once

// The checkbox records intent; the post-onboarding glue consumes the latch exactly once
// and fires `set_carddav_enabled(true)`. Consume-once is what keeps an opt-out from being
// silently re-enabled on the next launch (the same guarantee the mail/caldav/webdav
// latches carry).
@Test @MainActor func pendingCarddavEnableDefaultsOffAndIsIndependentOfMailAndCaldavAndWebdav() {
    let session = SessionState()
    #expect(session.pendingCarddavEnable == false)
    #expect(session.pendingCaldavEnable == false)
    #expect(session.pendingWebdavEnable == false)
    #expect(session.pendingFirstSetupMail == nil)

    // CardDAV gates independently: latching it must not imply mail, calendar, or files (a
    // contacts-only deployment fires only the carddav glue).
    session.pendingCarddavEnable = true
    #expect(session.pendingCaldavEnable == false)
    #expect(session.pendingWebdavEnable == false)
    #expect(session.pendingFirstSetupMail == nil)
}
