import XCTest

final class ContactsE2ETests: XCTestCase {
    private var app: XCUIApplication!
    private var config: E2EConfig!

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
        config = E2EConfig.load()
        app = AppHelper.launchApp()
    }

    override func tearDown() {
        app.terminate()
        super.tearDown()
    }

    func testContactAppears() {
        AppHelper.onboard(app: app, config: config)
        let contactsTab = app.tabBars.buttons["Contacts"]
        contactsTab.tap()
        let contactRow = app.staticTexts.matching(identifier: "contact-row").firstMatch
        XCTAssertTrue(
            contactRow.waitForExistence(timeout: 10),
            "Expected at least one contact to appear in the contacts list"
        )
    }
}
