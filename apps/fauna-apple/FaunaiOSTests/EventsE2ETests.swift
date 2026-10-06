import XCTest

final class EventsE2ETests: XCTestCase {
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

    func testEventAppearsInCalendar() {
        AppHelper.onboard(app: app, config: config)

        let eventsTab = app.tabBars.buttons["Events"]
        eventsTab.tap()

        let eventRow = app.buttons["event-row"]
        XCTAssertTrue(eventRow.waitForExistence(timeout: 15), "Expected at least one event to appear in the calendar")

        let eventText = app.staticTexts["E2E Test Event"]
        XCTAssertTrue(eventText.waitForExistence(timeout: 5), "Expected 'E2E Test Event' to be visible")
    }
}
