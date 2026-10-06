import XCTest

final class OnboardingE2ETests: XCTestCase {
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

    func testOnboardNewUser() {
        AppHelper.onboard(app: app, config: config)

        // Verify Messages tab is visible
        let messagesTab = app.tabBars.buttons["Messages"]
        XCTAssertTrue(messagesTab.exists, "Messages tab should be visible after onboarding")

        // Navigate to Settings and verify handle
        let settingsTab = app.tabBars.buttons["Settings"]
        settingsTab.tap()

        let handleLabel = app.staticTexts["settings-handle-label"]
        handleLabel.waitForLabel(containing: config.activeUser.handle)
    }
}
