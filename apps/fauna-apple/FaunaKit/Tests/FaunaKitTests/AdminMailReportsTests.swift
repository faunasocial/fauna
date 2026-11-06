import Testing
@testable import FaunaKit

// Guards for the `admin-mail-reports` page lifted to apple (dmarc-reporting.md
// § Admin pane summary surface). The window read, the fold and the shares are the
// shared `DmarcReportsMachine`'s; the cross-app render is
// `test_admin_mail_reports.py`. These cover the thin Swift seams: page registration
// and its place in the rail.

@Test func adminMailReportsPageIsBuiltAndCarriesTheUiYamlNavId() {
    #expect(AdminPage.mailReports.navId == "admin-mail-reports")
    #expect(AdminPage.mailReports.pageKey == "admin-mail-reports")
    #expect(AdminPage.built.contains(.mailReports))
    // Round-trips through the nav-id initializer the shell routes on.
    #expect(AdminPage(navId: "admin-mail-reports") == .mailReports)
}

// ui.yaml `navigation.admin_pages` lists it right after `admin-mail`.
@Test func adminMailReportsFollowsMailInTheSwitcherOrder() throws {
    let built = AdminPage.built
    let mail = try #require(built.firstIndex(of: .mail))
    let reports = try #require(built.firstIndex(of: .mailReports))
    #expect(reports == mail + 1)
}
