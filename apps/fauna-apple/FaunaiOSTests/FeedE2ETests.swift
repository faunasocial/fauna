import XCTest

final class FeedE2ETests: XCTestCase {
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

    func testComposePost() {
        AppHelper.onboard(app: app, config: config)

        let feedTab = app.tabBars.buttons["Feed"]
        feedTab.tap()

        let feedRow = app.buttons["feed-row"]
        feedRow.waitAndTap()

        let composeField = app.textFields["feed-compose-field"]
        composeField.waitAndTap()
        composeField.typeText("E2E test post")

        app.buttons["feed-send-button"].waitAndTap()

        let postRow = app.buttons["feed-post-row"]
        XCTAssertTrue(postRow.waitForExistence(timeout: 10), "Expected post to appear in feed")

        let postText = app.staticTexts["E2E test post"]
        XCTAssertTrue(postText.waitForExistence(timeout: 5), "Expected 'E2E test post' to be visible")
    }
}
